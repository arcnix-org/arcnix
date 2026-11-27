use std::collections::BTreeMap;

use arcnix_core::script_favorites::{self, Favorite};
use arcnix_core::script_parameters::Source;
use arcnix_core::scripts::{self, Template};
use arcnix_core::tasks::{self, Status, Task};
use zeroize::Zeroizing;

use crate::*;

#[derive(Clone, Default)]
pub(crate) struct ScriptsState {
    pub parameter_editor: crate::script_parameters_ui::EditorState,
    pub preset_selected: String,
    pub preset_name: String,
    pub exchange: crate::script_exchange_ui::ExchangeState,
    pub library: u8,
    pub search: String,
    pub template: Option<Template>,
    pub values: BTreeMap<String, String>,
    pub targets: HashSet<String>,
    pub target_filter: arcnix_core::target_selection::Filter,
    pub credentials: HashMap<String, (String, String)>,
    pub concurrency: usize,
    pub tasks: Vec<Task>,
    pub active: Option<String>,
    pub prepared: bool,
    pub submitted: bool,
    pub auto_submit: bool,
    pub software_task: Option<String>,
    pub selected_task: Option<String>,
    pub error: String,
    pub history_writable: bool,
    pub refresh_secret: String,
    pub refreshing: HashSet<(String, String)>,
    pub batch_refresh: crate::task_refresh_ui::RefreshState,
    pub private_previews: HashMap<(String, String), String>,
    /// Why a configuration preview failed, from the server; session only.
    pub private_failures: HashMap<(String, String), String>,
}
pub(crate) fn busy(s: &State) -> bool {
    s.scripts.active.is_some()
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Submission {
    ReviewBatch,
    ReviewSoftware,
    ApplySoftware,
}
fn templates(s: &State) -> Vec<Template> {
    let custom = || s.config.snippets.iter().cloned().map(Template::custom);
    let items: Vec<_> = match s.scripts.library {
        0 => scripts::catalog(),
        1 => custom().collect(),
        _ => scripts::catalog()
            .into_iter()
            .chain(custom())
            .filter(|t| script_favorites::contains(&s.config, t.source(), &t.snippet.id))
            .collect(),
    };
    let query = s.scripts.search.to_lowercase();
    items
        .into_iter()
        .filter(|t| {
            format!(
                "{} {} {}",
                t.snippet.name,
                t.snippet.description,
                if t.source() == Source::Official {
                    format!(
                        "{} {}",
                        i18n::translate(&t.snippet.name),
                        i18n::translate(&t.snippet.description)
                    )
                } else {
                    String::new()
                }
            )
            .to_lowercase()
            .contains(&query)
        })
        .collect()
}
fn can_submit(s: &State) -> bool {
    s.scripts.prepared
        && !s.scripts.submitted
        && s.scripts.history_writable
        && s.scripts.tasks.iter().any(|task| {
            Some(&task.id) == s.scripts.active.as_ref()
                && task
                    .targets
                    .iter()
                    .any(|target| target.status == Status::Ready)
        })
}

impl Ui {
    fn toggle_script_favorite(&self, favorite: Favorite) {
        let result = (|| -> anyhow::Result<_> {
            anyhow::ensure!(self.writable.get(), "Configuration is read-only");
            let mut config = self.state.with(|s| s.config.clone());
            script_favorites::toggle(&mut config, favorite)?;
            self.store.save(&config)?;
            Ok(config)
        })();
        self.state.revise(|s| match result {
            Ok(config) => {
                s.config = config;
                s.scripts.error.clear();
            }
            Err(e) => s.scripts.error = format!("{e:#}"),
        });
    }
    pub(crate) fn choose_server_script(&self, template: Template) {
        let selected = self.state.with(|s| {
            if !s.connected || busy(s) {
                return None;
            }
            s.selected.clone()
        });
        let Some(selected) = selected else {
            self.error("Connect a server and wait for the current operation to finish");
            return;
        };
        self.open_script(template, Some(selected), Dialog::ScriptRun);
    }
    fn refresh_server_task(&self) {
        let result = self.state.with(|s| -> anyhow::Result<_> {
            let Some(Dialog::ScriptRefresh(task_id, target_id, desktop)) = &s.dialog else {
                anyhow::bail!("Choose a server task");
            };
            let request =
                crate::task_refresh_ui::request(s, task_id, target_id, &s.scripts.refresh_secret)?;
            Ok((task_id.clone(), request, *desktop))
        });
        match result {
            Err(e) => self.state.revise(|s| s.dialog_error = e.to_string()),
            Ok((task, request, desktop)) => {
                self.state.revise(|s| {
                    s.scripts
                        .refreshing
                        .insert((task.clone(), request.id.clone()));
                    s.scripts.refresh_secret.clear();
                    s.dialog = desktop.then(|| Dialog::DesktopTask(task.clone()));
                    s.dialog_error.clear();
                });
                batch_worker::refresh_many(task, vec![request], 1, self.batch_events.clone());
            }
        }
    }
    pub(crate) fn choose_script(&self, template: Template) {
        self.open_script(template, None, Dialog::ScriptRun);
    }
    pub(crate) fn open_script(&self, template: Template, target: Option<String>, dialog: Dialog) {
        if self.state.with(busy) {
            return;
        }
        match self.configure_script(template, target) {
            Ok(()) => {
                self.state.revise(|s| s.scripts.credentials.clear());
                self.open_dialog(dialog);
            }
            Err(e) => self.state.revise(|s| s.scripts.error = e.to_string()),
        }
    }
    pub(crate) fn configure_script(
        &self,
        mut template: Template,
        target: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(policy) = template.installation.as_ref().and_then(|d| d.policy) {
            template.set_installation_policy(policy)?;
        }
        let defaults =
            arcnix_core::script_parameters::defaults(&template.snippet).unwrap_or_default();
        let values = template
            .parameters()?
            .into_iter()
            .map(|key| {
                let value = defaults.get(&key).cloned().unwrap_or_default();
                (key, value)
            })
            .collect();
        self.state.revise(|s| {
            s.scripts.template = Some(template);
            s.scripts.preset_selected.clear();
            s.scripts.preset_name.clear();
            s.scripts.values = values;
            s.scripts.targets = target.into_iter().collect();
            s.scripts.target_filter = Default::default();
            s.scripts.error.clear();
            s.scripts.concurrency = 3;
        });
        Ok(())
    }
    pub(crate) fn prepare_script(&self, submission: Submission) {
        let software = submission != Submission::ReviewBatch
            || self.state.with(|s| {
                s.scripts
                    .template
                    .as_ref()
                    .is_some_and(|template| template.snippet.id == "builtin:compose-application")
            });
        let result = (|| -> anyhow::Result<_> {
            anyhow::ensure!(
                self.writable.get() && self.state.with(|s| s.scripts.history_writable),
                "Configuration or task history is read-only"
            );
            self.state.with(|s| {
                anyhow::ensure!(!busy(s) && !s.system_task_running, "Wait for the current operation to finish");
                if software {
                    anyhow::ensure!(s.connected && s.selected.as_ref().is_some_and(|id| s.scripts.targets.len() == 1 && s.scripts.targets.contains(id)), "Connect to the server again before continuing");
                }
                let mut template = s.scripts.template.clone().ok_or_else(|| anyhow::anyhow!("Choose a script"))?;
                anyhow::ensure!(task_target_picker::hidden(s)==0,"Review hidden selected servers before checking targets");
                arcnix_core::target_selection::validate(&s.config,&template,&s.scripts.targets)?;
                template.snippet = template.render(&s.scripts.values)?;
                if let Some(job)=&template.backup
                    && matches!(job.action,arcnix_core::backups::Action::Initialize)
                    && !job.policy.repository.starts_with('/') {
                    anyhow::ensure!(s.scripts.targets.len()==1,"Initialize a shared repository from one server at a time");
                }
                let mut endpoints = HashSet::new();
                let mut targets = Vec::new();
                let mut records = Vec::new();
                for profile in s.config.available_servers().filter(|p| s.scripts.targets.contains(&p.id)) {
                    anyhow::ensure!(endpoints.insert((profile.host.to_lowercase(), profile.port)), "Select each server endpoint only once");
                    let fingerprint = s.config.trusted_fingerprint(&profile.host, profile.port).ok_or_else(|| anyhow::anyhow!("Connect and save the host fingerprint first: {}", profile.name))?.to_owned();
                    if let Some(job)=&template.cleanup {job.target(profile,&fingerprint)?;}
                    if let Some(job)=&template.audit_cleanup {job.target(profile,&fingerprint)?;}
                    if let Some(job)=&template.recovery {job.target(profile,&fingerprint)?;}
                    let (secret, sudo) = s.scripts.credentials.get(&profile.id).cloned().unwrap_or_default();
                    anyhow::ensure!([&secret, &sudo].iter().all(|value| value.len() <= 1024 && !value.contains(['\0','\r','\n'])), "Invalid credential");
                    // Tasks never reuse the connection's password, and an empty
                    // one only comes back from the server as a generic refusal.
                    anyhow::ensure!(profile.auth != arcnix_core::AuthMethod::Password || !secret.is_empty(), "Enter the SSH password for {} in SSH password or key passphrase", profile.name);
                    let mut profile = profile.clone(); if profile.connection.is_none() { profile.connection=Some(s.config.settings.connection); }
                    records.push(tasks::Target { id: profile.id.clone(), name: profile.name.clone(), endpoint: format!("{}@{}:{}", profile.username, profile.host, profile.port), fingerprint: fingerprint.clone(), status: Status::Checking, preview: String::new(), output: String::new(), exit_code: None, remote_directory: String::new(), result_read_at: None });
                    targets.push(batch_worker::Target { profile, fingerprint, secret: Zeroizing::new(secret), sudo: Zeroizing::new(sudo) });
                }
                anyhow::ensure!(!targets.is_empty() && targets.len() <= 32, "Select between 1 and 32 servers");
                let task = Task { configuration_targets:template.configuration_records(),stops:None, drill: template.drill.as_ref().map(arcnix_core::recovery_drills::Job::description).transpose()?, installation: template.installation.clone(), configuration:template.configuration.as_ref().map(|j|j.record.clone()), diagnostic: None, backup: if let Some(job)=&template.drill {Some(arcnix_core::backup_evidence::TaskInfo::from_drill(job)?)}else{template.backup.as_ref().and_then(arcnix_core::backup_evidence::TaskInfo::from_job)}, id: new_id(), name: template.snippet.name.clone(), script_id: template.snippet.id.clone(), revision: template.revision.clone(), created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs(), targets: records };
                Ok((task, template, targets, s.scripts.concurrency.clamp(1,8)))
            })
        })();
        match result {
            Err(e) => self.state.revise(|s| s.dialog_error = format!("{e:#}")),
            Ok((task, template, targets, concurrency)) => {
                let id = task.id.clone();
                let mut history = self.state.with(|s| s.scripts.tasks.clone());
                history.insert(0, task);
                history.truncate(10);
                if let Err(e) = tasks::History::for_config(self.store.path()).save(&history) {
                    self.state.revise(|s| s.dialog_error = e.to_string());
                    return;
                }
                self.state.revise(|s| {
                    s.scripts.tasks = history;
                    s.scripts.private_previews.clear();
                    s.scripts.private_failures.clear();
                    s.scripts.active = Some(id.clone());
                    s.scripts.selected_task = Some(id.clone());
                    s.scripts.prepared = false;
                    s.scripts.submitted = false;
                    s.scripts.auto_submit = submission == Submission::ApplySoftware;
                    if software {
                        s.scripts.software_task = Some(id.clone());
                    }
                    if s.dialog == Some(Dialog::SoftwareInstall) {
                        s.install.task = Some(id.clone());
                    }
                    s.scripts.credentials.clear();
                    s.scripts.values.clear();
                    s.scripts.preset_selected.clear();
                    s.scripts.preset_name.clear();
                    if software || server_desktop::visible(s) {
                        show_dialog(s, Dialog::DesktopTask(id.clone()));
                    } else {
                        task_records_ui::recent(s);
                        s.dialog = None;
                        navigation::change_page(s, navigation::Page::Tasks, "prepare-script");
                    }
                });
                let controller = batch_worker::start(
                    id,
                    template,
                    targets,
                    concurrency,
                    self.batch_events.clone(),
                );
                *self.batch_controller.borrow_mut() = Some(controller);
            }
        }
    }
    pub(crate) fn persist_tasks(&self) -> bool {
        let result = tasks::History::for_config(self.store.path())
            .save(&self.state.with(|s| s.scripts.tasks.clone()));
        if let Err(e) = result {
            self.state.revise(|s| {
                s.scripts.history_writable = false;
                s.scripts.error = format!("Cannot save task history: {e:#}");
            });
            false
        } else {
            true
        }
    }
    fn submit_script(&self) {
        if !self.state.with(can_submit) {
            return;
        }
        self.state.revise(|s| {
            if let Some(task) = s
                .scripts
                .tasks
                .iter_mut()
                .find(|t| Some(&t.id) == s.scripts.active.as_ref())
            {
                for target in &mut task.targets {
                    if target.status == Status::Ready {
                        target.status = Status::Queued;
                    }
                }
            }
        });
        if !self.persist_tasks() {
            self.stop_script_queue();
            return;
        }
        if let Some(controller) = self.batch_controller.borrow().as_ref() {
            let _ = controller.decision.send(true);
        }
        self.state.revise(|s| s.scripts.submitted = true);
    }
    pub(crate) fn stop_script_queue(&self) {
        if let Some(controller) = self.batch_controller.borrow().as_ref() {
            controller.stop.store(true, Ordering::Release);
            let _ = controller.decision.send(false);
        }
    }
}
pub(crate) fn event(ui: &Ui, event: batch_worker::Event) {
    let event = match event {
        event @ (batch_worker::Event::CleanupRecords { .. }
        | batch_worker::Event::ExportDownload { .. }) => {
            cleanup_records_ui::event(ui, event);
            return;
        }
        event @ batch_worker::Event::RecordStorage { .. } => {
            record_maintenance_ui::event(ui, event);
            return;
        }
        event @ (batch_worker::Event::TaskStop { .. }
        | batch_worker::Event::NativeTask { .. }
        | batch_worker::Event::RecordPage { .. }
        | batch_worker::Event::RecordDetail { .. }) => {
            task_records_ui::event(ui, event);
            return;
        }
        batch_worker::Event::Snapshots(serial, result) => {
            backups_ui::snapshots_event(ui, serial, result);
            return;
        }
        batch_worker::Event::SnapshotFiles(serial, result) => {
            backups_ui::files_event(ui, serial, result);
            return;
        }
        event => event,
    };
    let automatic = matches!(&event, batch_worker::Event::Prepared(id) if ui.state.with(|s| {
        s.scripts.auto_submit && s.scripts.active.as_ref() == Some(id)
            && matches!(&s.dialog, Some(Dialog::DesktopTask(shown)) if shown == id)
    }));
    let refresh_software = match &event {
        batch_worker::Event::Complete(id) | batch_worker::Event::Refreshed { task: id, .. } => {
            ui.state.with(|s| {
                s.connected
                    && s.scripts.software_task.as_ref() == Some(id)
                    && s.scripts
                        .tasks
                        .iter()
                        .find(|task| &task.id == id)
                        .is_some_and(|task| {
                            task.targets.iter().any(|target| {
                                s.selected.as_ref() == Some(&target.id)
                                    && target.may_have_server_result()
                            })
                        })
            })
        }
        _ => false,
    };
    // The Docker app checks for itself whether a finished task concerns it.
    let finished = match &event {
        batch_worker::Event::Complete(id) => Some(id.clone()),
        _ => None,
    };
    // Service, site and configuration tasks can change what Nginx shows.
    let refresh_nginx = match &event {
        batch_worker::Event::Complete(id)
            if ui.state.with(|s| {
                s.scripts
                    .tasks
                    .iter()
                    .find(|task| &task.id == id)
                    .is_some_and(|task| {
                        (task.script_id.starts_with("builtin:nginx:")
                            || task.script_id.starts_with("builtin:redis:")
                            || task.script_id == arcnix_core::configurations::SCRIPT_ID
                            || task.script_id == "builtin:service-operation")
                            && task
                                .targets
                                .iter()
                                .any(|target| s.selected.as_ref() == Some(&target.id))
                    })
            }) =>
        {
            Some(id.clone())
        }
        _ => None,
    };
    ui.state.revise(|s| match event {
        batch_worker::Event::ConfigurationPreview { task, id, diff } => {
            s.scripts.private_previews.insert((task, id), diff);
        }
        batch_worker::Event::ConfigurationFailure { task, id, detail } => {
            s.scripts.private_failures.insert((task, id), detail);
        }
        batch_worker::Event::Observed { task, id, snapshot } => {
            if let Some(record) = s.scripts.tasks.iter_mut().find(|record| record.id == task) {
                apply_refresh(record, &id, Ok(snapshot));
            }
        }
        batch_worker::Event::Snapshots(..) => unreachable!(),
        batch_worker::Event::SnapshotFiles(..) => unreachable!(),
        batch_worker::Event::RecordPage { .. } | batch_worker::Event::RecordDetail { .. } => {
            unreachable!()
        }
        batch_worker::Event::RecordStorage { .. } => unreachable!(),
        batch_worker::Event::CleanupRecords { .. } => unreachable!(),
        batch_worker::Event::ExportDownload { .. } => unreachable!(),
        batch_worker::Event::TaskStop { .. } | batch_worker::Event::NativeTask { .. } => {
            unreachable!()
        }
        batch_worker::Event::Refreshed { task, id, result } => {
            s.scripts.refreshing.remove(&(task.clone(), id.clone()));
            if let Some(record) = s.scripts.tasks.iter_mut().find(|record| record.id == task) {
                apply_refresh(record, &id, result);
            }
        }

        batch_worker::Event::Directory {
            task,
            id,
            directory,
        } => {
            if let Some(t) = s
                .scripts
                .tasks
                .iter_mut()
                .find(|t| t.id == task)
                .and_then(|t| t.targets.iter_mut().find(|t| t.id == id))
            {
                t.remote_directory = directory;
            }
        }
        batch_worker::Event::Target {
            task,
            id,
            status,
            preview,
            output,
            code,
        } => {
            if let Some(t) = s
                .scripts
                .tasks
                .iter_mut()
                .find(|t| t.id == task)
                .and_then(|t| t.targets.iter_mut().find(|t| t.id == id))
            {
                t.status = status;
                if let Some(preview) = preview {
                    t.preview = preview;
                }
                tasks::append(&mut t.output, &output);
                t.exit_code = code;
            }
            if let Some(record) = s.scripts.tasks.iter_mut().find(|record| record.id == task) {
                record.confirm_required_checks();
            }
        }
        batch_worker::Event::Prepared(id) => {
            if s.scripts.active.as_ref() == Some(&id) {
                s.scripts.prepared = true;
            }
        }
        batch_worker::Event::Complete(id) => {
            if s.scripts.active.as_ref() == Some(&id) {
                s.scripts.active = None;
                s.scripts.prepared = false;
                s.scripts.submitted = false;
                s.scripts.auto_submit = false;
            }
        }
    });
    backups_ui::record_confirmations(ui);
    ui.persist_tasks();
    if automatic {
        if ui.state.with(|s| s.scripts.history_writable) {
            ui.submit_script();
        } else {
            ui.stop_script_queue();
        }
    }
    if !ui.state.with(busy) {
        ui.batch_controller.borrow_mut().take();
    }
    if let Some(id) = refresh_nginx {
        ui.nginx_task_finished(&id);
        ui.redis_task_finished(&id);
        ui.docker_task_finished(&id);
    }
    if let Some(id) = finished {
        ui.docker_pull_finished(&id);
        ui.docker_run_finished(&id);
    }
    if refresh_software {
        ui.refresh_software_inventory();
    }
}
fn apply_refresh(
    record: &mut Task,
    id: &str,
    result: std::result::Result<batch_worker::Snapshot, String>,
) {
    match result {
        Ok(snapshot) => apply_observation(record, id, &snapshot.observation, snapshot.received_at),
        Err(error) => {
            if let Some(target) = record.targets.iter_mut().find(|target| target.id == id) {
                target.status = Status::Unknown;
                target.exit_code = None;
                tasks::append(&mut target.output, &format!("\nQuery failed: {error}\n"));
            }
        }
    }
}
fn apply_observation(
    record: &mut Task,
    id: &str,
    observation: &arcnix_core::remote_tasks::Observation,
    received_at: Option<u64>,
) {
    if let Some(target) = record.targets.iter_mut().find(|target| target.id == id) {
        target.status = observation.status();
        target.result_read_at = received_at;
        target.exit_code = observation.confirmed_exit();
        target.output.clear();
        tasks::append(&mut target.output, &observation.output);
        let reason = observation.reason();
        if !reason.is_empty() {
            tasks::append(
                &mut target.output,
                &format!("\n{}\n", i18n::translate(reason)),
            );
        }
    }
    record.confirm_required_checks();
}

pub(crate) fn restore(ui: &Ui) {
    match tasks::History::for_config(ui.store.path()).load() {
        Ok(tasks) => ui.state.revise(|s| {
            s.scripts.tasks = tasks;
            s.scripts.history_writable = true;
        }),
        Err(e) => ui.state.revise(|s| {
            s.scripts.error = format!("{e:#}");
            s.scripts.history_writable = false;
        }),
    }
}
pub(crate) fn sidebar(ui: Ui) -> impl Widget {
    let state = ui.state;
    col().class("context-sidebar").fill((
        row().class("context-title help-heading").fill((text().content(i18n::bind(state.map(|s| if s.navigation==navigation::Page::Scripts {"Scripts"} else {"Tasks"}.to_owned()))),crate::help_ui::group("Server tasks",&["Select targets when running a script. No active server selection is required.","Jobs run as server-owned systemd services. Closing Arcnix does not stop submitted jobs.","Favorites reference the current script, without pinning an older version. Review targets and changes before running."]))),
        when(state.map(|s|s.navigation==navigation::Page::Scripts), {let ui=ui.clone();move||{
            col().class("context-links").fill(each(vec![(0,"Official scripts"),(1,"My scripts"),(2,"Favorites")]).key(|x|x.0).children({let ui=ui.clone();move|item|{
                let (index,label)=item.value.get();let choose=ui.clone();
                btn(label,move||choose.state.revise(|s|s.scripts.library=index)).class(state.map(move|s| if s.scripts.library==index {"context-link active"} else {"context-link"}.to_owned()))
            }}))
        }}),
    ))
}
fn favorite_button(ui: Ui, favorite: Favorite) -> impl Widget {
    let state = ui.state;
    let name = favorite.clone();
    let glyph = favorite.clone();
    let style = favorite.clone();
    sapid::button()
        .name(i18n::bind(state.map(move |s| {
            if script_favorites::contains(&s.config, name.source, &name.script) {
                "Remove from favorites"
            } else {
                "Add to favorites"
            }
            .to_owned()
        })))
        .class(state.map(move |s| {
            if script_favorites::contains(&s.config, style.source, &style.script) {
                "icon-button script-favorite active"
            } else {
                "icon-button script-favorite"
            }
            .to_owned()
        }))
        .enabled(ui.writable)
        .fill(
            text()
                .class(state.map(move |s| {
                    if script_favorites::contains(&s.config, glyph.source, &glyph.script) {
                        "action-glyph glyph-star-filled"
                    } else {
                        "action-glyph glyph-star"
                    }
                    .to_owned()
                }))
                .content(""),
        )
        .on_press(move || ui.toggle_script_favorite(favorite.clone()))
}
pub(crate) fn page(ui: Ui) -> impl Widget {
    let state = ui.state;
    let create = ui.clone();
    col().class("script-page page-scroll").fill((
        row().class("global-toolbar").fill((
            text()
                .class("field-label")
                .content(i18n::bind("Search scripts")),
            input()
                .name(i18n::bind("Search scripts"))
                .value(state.map(|s| s.scripts.search.clone()))
                .on(value_changed, move |e| {
                    if let Event::ValueChanged { value, .. } = e {
                        state.revise(|s| s.scripts.search = value.clone());
                    }
                }),
            text_action("New script", Icon::Plus)
                .class("button with-icon primary")
                .on_press(move || create.edit_snippet(None)),
            crate::script_exchange_ui::toolbar(ui.clone()),
        )),
        text()
            .class(state.map(|s| {
                if s.scripts.error.is_empty() {
                    "hidden"
                } else {
                    "form-error"
                }
                .to_owned()
            }))
            .content(state.map(|s| s.scripts.error.clone())),
        when(state.map(|s| templates(s).is_empty()), move || {
            text()
                .class("context-hint")
                .content(i18n::bind(state.map(|s| {
                    if !s.scripts.search.is_empty() {
                        "No matching scripts"
                    } else if s.scripts.library == 2 {
                        "No favorite scripts yet"
                    } else {
                        "No scripts found. Create a script or choose the official library."
                    }
                    .to_owned()
                })))
        }),
        each(state.map(templates))
            .key(move |t| {
                format!(
                    "{}:{:?}:{}:{}:{}:{}:{}:{}",
                    state.with(|s| s.scripts.library),
                    t.source(),
                    t.revision,
                    t.snippet.id,
                    t.snippet.name,
                    t.snippet.description,
                    t.snippet.command,
                    t.snippet.administrator
                )
            })
            .children({
                let ui = ui.clone();
                move |item| {
                    let template = item.value.get();
                    let run = ui.clone();
                    let copy = ui.clone();
                    let edit = ui.clone();
                    let delete = ui.clone();
                    let name = template.snippet.name.clone();
                    let description = template.snippet.description.clone();
                    let run_template = template.clone();
                    let copy_template = template.clone();
                    let edit_template = template.clone();
                    let id = template.snippet.id.clone();
                    let official = template.source() == Source::Official;
                    let favorite =
                        favorite_button(ui.clone(), Favorite::new(template.source(), &id));
                    col().class("script-row").fill((
                        row().class("script-row-heading").fill((
                            row().class("help-heading").fill((
                                favorite,
                                text().class("section-title").content(if official {
                                    i18n::translate(&name)
                                } else {
                                    name
                                }),
                                when(Vary::Value(template.installation.is_some()), {
                                    let scope = template
                                        .installation
                                        .as_ref()
                                        .map(|d| d.scope().to_owned())
                                        .unwrap_or_default();
                                    move || {
                                        crate::help_ui::tip("Installation checks", scope.clone())
                                    }
                                }),
                            )),
                            text().class("subtle").content(if official {
                                i18n::translate(&format!("Version {}", template.revision))
                            } else {
                                "Shell".into()
                            }),
                        )),
                        text().class("subtle").content(if official {
                            i18n::translate(&description)
                        } else {
                            description
                        }),
                        row().class("script-actions").fill((
                            text_action("Run on servers…", Icon::Play)
                                .class("button with-icon")
                                .enabled(state.map(|s| !busy(s)))
                                .on_press(move || run.choose_script(run_template.clone())),
                            btn("Copy to my scripts", move || {
                                let mut snippet = copy_template.snippet.clone();
                                snippet.id = new_id();
                                copy.edit_snippet(Some(snippet));
                            }),
                            when(Vary::Value(!official), move || {
                                let edit = edit.clone();
                                let delete = delete.clone();
                                let edit_template = edit_template.clone();
                                let id = id.clone();
                                fragment((
                                    btn("Edit", move || {
                                        edit.edit_snippet(Some(edit_template.snippet.clone()))
                                    }),
                                    btn("Delete", move || {
                                        delete.open_dialog(Dialog::SnippetDelete(id.clone()))
                                    }),
                                ))
                            }),
                        )),
                    ))
                }
            }),
    ))
}
pub(crate) fn tasks_page(ui: Ui) -> impl Widget {
    let state = ui.state;
    let reports = ui.clone();
    col().class("tasks-page page-scroll").fill((
        task_records_ui::navigation(ui.clone()),
        when(
            state.map(|s| s.record_storage.visible && !s.cleanup_reports.visible),
            {
                let ui = ui.clone();
                move || record_maintenance_ui::page(ui.clone())
            },
        ),
        when(
            state.map(|s| {
                !s.record_storage.visible && !s.cleanup_reports.visible && s.task_records.visible
            }),
            {
                let ui = ui.clone();
                move || task_records_ui::page(ui.clone())
            },
        ),
        when(
            state.map(|s| {
                !s.record_storage.visible && !s.task_records.visible && !s.cleanup_reports.visible
            }),
            move || recent_tasks(ui.clone()),
        ),
        when(state.map(|s| s.cleanup_reports.visible), move || {
            cleanup_records_ui::page(reports.clone())
        }),
    ))
}
fn recent_tasks(ui: Ui) -> impl Widget {
    let state = ui.state;
    let submit = ui.clone();
    let stop = ui.clone();
    col().class("recent-tasks").fill((
        row().class("help-heading").fill((text().class("field-label").content(i18n::bind("Recent batches")),crate::help_ui::tip("Recent batches","Recent 10 tasks are stored locally. Output is a bounded tail; full logs and results remain on each server."))),
        when(state.map(|s|s.scripts.tasks.iter().any(|t|t.targets.iter().any(|t|t.status==Status::Submitting))),||text().class("hint").content(i18n::bind("Submitting means the server has not acknowledged receipt. Closing Arcnix may interrupt that submission."))),
        text().class(state.map(|s|if s.scripts.error.is_empty(){"hidden"}else{"form-error"}.to_owned())).content(state.map(|s|s.scripts.error.clone())),
        when(state.map(|s|s.scripts.tasks.is_empty()),||text().class("context-hint").content(i18n::bind("Run a script to create your first task."))),
        when(state.map(busy),move||{let submit=submit.clone();let stop=stop.clone();row().class("global-toolbar").fill((
            btn("Execute ready targets",move||submit.submit_script()).class("button primary").enabled(state.map(can_submit)),
            btn("Stop pending targets",move||stop.stop_script_queue()),
            text().class("subtle").content(i18n::bind("Already submitted server jobs continue running.")),
        ))}),
        each(state.map(|s|s.scripts.tasks.clone())).key(|t|t.id.clone()).children({let ui=ui.clone();move|item|{
            let task=item.value.get();let id=task.id.clone();let choose=ui.clone();let ui=ui.clone();let task_id=task.id.clone();let batch_ui=ui.clone();let batch_id=task.id.clone();let batch_enabled=task.id.clone();let batch_visible=task.id.clone();
            col().class("task-entry").fill((
                btn(format!("{} · {} · {}",task.name,formatting::timestamp(Some(task.created)),task.targets.len()),move||choose.state.revise(|s|s.scripts.selected_task=Some(id.clone()))).class("task-heading"),
                btn("Query batch results…",move||batch_ui.open_batch_refresh(&batch_id)).class(state.map(move|s|if s.scripts.selected_task.as_ref()==Some(&batch_visible){"button task-batch-refresh"}else{"hidden"}.to_owned())).enabled(state.map(move|s|!busy(s)&&s.scripts.tasks.iter().find(|t|t.id==batch_enabled).is_some_and(|t|t.targets.iter().any(|target|crate::task_refresh_ui::queryable(s,&t.id,target))))),
                when(state.map(move|s|s.scripts.selected_task.as_ref()==Some(&task_id)),move||{
                    let target_task_id=task.id.clone();let query_ui=ui.clone();let query_task=task.id.clone();let current_revision=task.revision.clone();let current_definition=task.installation.clone();let current_drill=task.drill.clone();let current_stops=task.stops.clone();
                    col().class("task-targets").fill(each(state.map(move|s|s.scripts.tasks.iter().find(|t|t.id==target_task_id).map(|t|t.targets.clone()).unwrap_or_default())).key(|t|t.id.clone()).children(move|target|{
                        let t=target.value;let query=query_ui.clone();let query_task=query_task.clone();let target_id=t.get().id;let enabled_task=query_task.clone();let enabled_id=target_id.clone();let pending_task=query_task.clone();let pending_id=target_id.clone();let diff_task=query_task.clone();let diff_id=target_id.clone();
                        let install_id=query_task.clone();let install_revision=current_revision.clone();let install_definition=current_definition.clone();let has_installation=current_definition.is_some();
                        let drill_id=query_task.clone();let drill_revision=current_revision.clone();let drill_description=current_drill.clone();let has_drill=current_drill.is_some();let stop_id=query_task.clone();let stop_revision=current_revision.clone();let stop_description=current_stops.as_ref().and_then(|descriptions|descriptions.get(&target_id)).cloned();let has_stops=stop_description.is_some();
                        col().class("task-target").fill((
                            row().class("script-row-heading").fill((text().class("section-title").content(t.map(|t|t.name.clone())),text().class("task-status").content(i18n::bind(t.map(|t|t.status.label().to_owned()))))),
                            text().class("subtle").content(t.map(|t|t.endpoint.clone())),
                            text().class("subtle").content(t.map(|t|format!("{} · {}",t.fingerprint,t.remote_directory))),
                            row().class("script-actions").fill((btn("Query server result",move||{query.state.revise(|s|s.scripts.refresh_secret.clear());query.open_dialog(Dialog::ScriptRefresh(query_task.clone(),target_id.clone(),false));}).enabled(state.map(move|s|!busy(s)&&!s.scripts.refreshing.contains(&(enabled_task.clone(),enabled_id.clone()))&&s.scripts.tasks.iter().find(|task|task.id==enabled_task).and_then(|task|task.targets.iter().find(|target|target.id==enabled_id)).is_some_and(tasks::Target::may_have_server_result))),
                            text().class("task-query-progress subtle").content(i18n::bind(state.map(move|s|if s.scripts.refreshing.contains(&(pending_task.clone(),pending_id.clone())){"Reading server result…"}else{""}.to_owned()))),
                            row().class(t.map(|t|if t.remote_directory.is_empty() || t.result_read_at.is_none() && matches!(t.status,Status::Checking|Status::Ready|Status::Queued|Status::Submitting|Status::Cancelled){"hidden"}else{"help-heading task-result-age"}.to_owned())).fill((text().class("subtle task-result-read").content(t.map(|t|t.result_read_at.map(|at|i18n::translate(&format!("Snapshot read: {}",formatting::timestamp(Some(at))))).unwrap_or_else(||i18n::translate("Read time unavailable")))),crate::help_ui::tip("Result snapshot","Time this client received the displayed server snapshot, using this computer's clock. This is not the task completion time or continuous confirmation of its state. Failed queries retain the previous read time. Older history may have no recorded time."))),
                            )),
                            text().class("script-code").selectable(true).content(t.map(|t|t.preview.clone())),
                            text().class("script-code configuration-diff").selectable(true).content(state.map(move|s|configuration_report(s,&(diff_task.clone(),diff_id.clone())))),
                            text().class("subtle").content(t.map(|t|arcnix_core::backups::progress(&t.output).map(|p|i18n::translate(&format!("{:.0}% · {} / {} · {} / {} files",p.fraction*100.0,formatting::bytes(p.bytes),formatting::bytes(p.total_bytes),p.files,p.total_files))).unwrap_or_default())),
                            col().class(t.map(|t|if arcnix_core::backups::progress(&t.output).is_some(){"backup-progress-track"}else{"hidden"}.to_owned())).name(i18n::bind("Backup progress")).role(glory::render::Role::ProgressBar).semantics(t.map(|t|Some(Box::new(glory::render::Semantics{progress:Some(glory::render::ProgressValue{min:0.0,max:100.0,current:arcnix_core::backups::progress(&t.output).map(|p|p.fraction*100.0),value_text:None}),..Default::default()})))).fill(col().class("backup-progress-fill").style(t.map(|t|glory::style::Style::new().width(glory::style::Length::Percent(arcnix_core::backups::progress(&t.output).map(|p|p.fraction*100.0).unwrap_or(0.0)))))),
                            text().class("script-code installation-report").selectable(true).content(t.map(move|t|if has_installation {crate::installation_ui::summary(&t.output,&install_id,&install_revision,install_definition.as_ref(),matches!(t.status,Status::Succeeded|Status::Failed).then_some(t.exit_code).flatten())}else{String::new()})),
                            fragment((text().class("script-code task-stop-report").selectable(true).content(t.map(move|t|task_records_ui::stop_report(&t.output,&stop_id,&stop_revision,stop_description.as_ref()))),
                            text().class("script-code drill-report").selectable(true).content(t.map(move|t|if has_drill {crate::recovery_ui::summary(&t.output,&drill_id,&drill_revision,Some(&t.id),drill_description.as_ref(),t.exit_code,matches!(t.status,Status::Succeeded|Status::Failed))}else{String::new()})))),

                            crate::log_text::output_view("script-code task-output",t.map(move|t|crate::log_text::shown(if has_stops {arcnix_core::task_stops::human_log(&t.output)}else if has_installation {arcnix_core::installers::human_log(&t.output)}else if has_drill {arcnix_core::recovery_drills::human_log(&t.output)}else{t.output.clone()}))),
                            text().class("subtle").content(t.map(|t|t.exit_code.map(|code|format!("Exit code: {code}")).unwrap_or_default())),
                        ))
                    }))
                }),
            ))
        }}),
    ))
}
fn desktop_task(ui: Ui, id: String) -> impl Widget {
    let state = ui.state;
    let metadata = state.with(|s| {
        s.scripts
            .tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| {
                (
                    task.name.clone(),
                    task.installation.clone(),
                    task.revision.clone(),
                )
            })
    });
    let (name, installation, revision) = metadata.unwrap_or_default();
    let task_id = id.clone();
    let targets = state.map(move |s| {
        s.scripts
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .map(|task| task.targets.clone())
            .unwrap_or_default()
    });
    let active_id = id.clone();
    let awaiting_submit = state.map(move |s| {
        if s.scripts.active.as_ref() == Some(&active_id) && !s.scripts.submitted {
            "Cancel"
        } else {
            "Close"
        }
        .to_owned()
    });
    let submit_id = id.clone();
    let back_id = id.clone();
    let back = ui.clone();
    let submit = ui.clone();
    let close = ui.clone();
    let submit_label = if installation.is_some()
        && state.with(|s| s.scripts.software_task.as_ref() == Some(&id))
    {
        "Apply package changes"
    } else {
        "Run operation"
    };
    modal(
        ui.clone(),
        "Operation status",
        col().class("desktop-task").fill((
            text().class("section-title").content(i18n::bind(name)),
            text()
                .class("script-code")
                .content(state.map(|s| s.scripts.error.clone())),
            each(targets).key(|t| t.id.clone()).children(move |item| {
                let target = item.value;
                let target_id = target.get().id;
                let query_key = (id.clone(), target_id.clone());
                let query = ui.clone();
                let queryable = id.clone();
                let open = id.clone();
                let report_id = id.clone();
                let review_id = id.clone();
                let definition = installation.clone();
                let revision = revision.clone();
                col().class("task-target").fill((
                    row().class("script-row-heading").fill((
                        text().content(target.map(|t| t.name.clone())),
                        text()
                            .class("task-status")
                            .content(i18n::bind(target.map(|t| t.status.label().to_owned()))),
                    )),
                    text()
                        .class("subtle")
                        .content(target.map(|t| t.endpoint.clone())),
                    when(
                        state.map(move |s| {
                            s.scripts.active.as_ref() == Some(&review_id)
                                && !s.scripts.auto_submit
                                && !s.scripts.submitted
                        }),
                        move || {
                            text()
                                .class("script-code")
                                .selectable(true)
                                .content(target.map(|t| t.preview.clone()))
                        },
                    ),
                    text()
                        .class("script-code configuration-diff")
                        .selectable(true)
                        .content(state.map({
                            let key = (id.clone(), target_id.clone());
                            move |s| configuration_report(s, &key)
                        })),
                    text()
                        .class("script-code installation-report")
                        .selectable(true)
                        .content(target.map(move |t| {
                            let Some(definition) = &definition else {
                                return String::new();
                            };
                            let exit_code = matches!(t.status, Status::Succeeded | Status::Failed)
                                .then_some(t.exit_code)
                                .flatten();
                            installation_ui::summary(
                                &t.output,
                                &report_id,
                                &revision,
                                Some(definition),
                                exit_code,
                            )
                        })),
                    crate::log_text::output_view(
                        "script-code task-output",
                        target.map(|t| crate::log_text::shown(desktop_output(t))),
                    ),
                    when(
                        state.map(move |s| {
                            !busy(s) && task_refresh_ui::queryable(s, &queryable, &target.get())
                        }),
                        move || {
                            let query = query.clone();
                            let id = open.clone();
                            let target = target_id.clone();
                            btn("Query server result", move || {
                                query.state.revise(|s| s.scripts.refresh_secret.clear());
                                query.open_dialog(Dialog::ScriptRefresh(
                                    id.clone(),
                                    target.clone(),
                                    true,
                                ));
                            })
                        },
                    ),
                    when(
                        state.map(move |s| s.scripts.refreshing.contains(&query_key)),
                        || {
                            text()
                                .class("subtle")
                                .content(i18n::bind("Reading server result…"))
                        },
                    ),
                ))
            }),
        )),
        (
            when(
                state.map(move |s| {
                    !busy(s)
                        && s.connected
                        && s.install.task.as_ref() == Some(&back_id)
                        && s.scripts
                            .tasks
                            .iter()
                            .find(|task| task.id == back_id)
                            .is_some_and(|task| {
                                task.targets.iter().any(|target| {
                                    target.status == Status::Failed
                                        && !target.may_have_server_result()
                                })
                            })
                }),
                move || {
                    let back = back.clone();
                    btn("Back to installation", move || {
                        back.open_dialog(Dialog::SoftwareInstall)
                    })
                },
            ),
            sapid::button()
                .class("button")
                .label(i18n::bind(awaiting_submit))
                .on_press(move || close.close_dialog()),
            when(
                state.map(move |s| {
                    s.scripts.active.as_ref() == Some(&submit_id)
                        && !s.scripts.auto_submit
                        && can_submit(s)
                }),
                move || {
                    let submit = submit.clone();
                    btn(submit_label, move || submit.submit_script()).class("button primary")
                },
            ),
        ),
    )
}

/// What a configuration task's operator should read: how it ended, then the
/// server's reason for a failed check or the diff under review. The reason
/// and the diff live in session memory only.
fn configuration_report(s: &State, key: &(String, String)) -> String {
    let outcome = s
        .scripts
        .tasks
        .iter()
        .find(|task| task.id == key.0)
        .and_then(|task| task.targets.iter().find(|target| target.id == key.1))
        .and_then(|target| arcnix_core::configurations::outcome(&target.output))
        .map(i18n::translate);
    let detail = s
        .scripts
        .private_failures
        .get(key)
        .map(|d| {
            format!(
                "{}\n{d}",
                i18n::translate("Server details, shown for this session only")
            )
        })
        .or_else(|| {
            s.scripts.private_previews.get(key).map(|d| {
                format!(
                    "{}\n{d}",
                    i18n::translate("Session-only configuration diff")
                )
            })
        });
    [outcome, detail]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn desktop_output(target: &tasks::Target) -> String {
    let output = target.output.trim();
    match output {
        "Preflight failed: Server-owned jobs require a running systemd manager and systemd-run" => {
            i18n::translate(
                "The operation did not start. This server needs a running systemd system manager and systemd-run.",
            )
        }
        "Preflight failed: The plan changes an installed package; select Allow reviewed upgrades and check targets again"
        | "Preflight failed: The plan configures an existing package; select Allow reviewed upgrades and check targets again" => {
            i18n::translate(
                "Installation requires changes to installed packages. Open Installation options and select Review dependency upgrades to see the changes.",
            )
        }
        _ => arcnix_core::installers::human_log(&target.output),
    }
}

pub(crate) fn dialogs(ui: Ui) -> impl Widget {
    let state = ui.state;
    fragment((
        crate::task_refresh_ui::dialog(ui.clone()),
        each(state.map(|s| match &s.dialog {
            Some(Dialog::DesktopTask(id)) => vec![id.clone()],
            _ => vec![],
        }))
        .key(Clone::clone)
        .children({
            let ui = ui.clone();
            move |item| desktop_task(ui.clone(), item.value.get())
        }),
        when(
            state.map(|s| matches!(s.dialog, Some(Dialog::ScriptRefresh(..)))),
            {
                let ui = ui.clone();
                move || {
                    let submit = ui.clone();
                    let cancel = ui.clone();
                    modal(ui.clone(),"Query server result",col().class("form-stack").fill((text().class("hint").content(i18n::bind("Reconnect to read server-owned logs and the exit result. This does not submit or repeat the task.")),text().class("field-label").content(i18n::bind("SSH password or key passphrase")),input().purpose(Purpose::Password).name(i18n::bind("SSH password or key passphrase")).value(state.map(|s|s.scripts.refresh_secret.clone())).on(value_changed,move|e|{if let Event::ValueChanged {value,..}=e{state.revise(|s|s.scripts.refresh_secret=value);}}))),(btn("Cancel",move||cancel.close_dialog()),btn("Query",move||submit.refresh_server_task()).class("button primary")))
                }
            },
        ),
        when(
            state.map(|s| s.dialog == Some(Dialog::ScriptRun)),
            move || {
                let prepare = ui.clone();
                let cancel = ui.clone();
                modal(ui.clone(),"Run script on servers",col().class("form-stack").fill((
            row().class("help-heading").fill((text().class("section-title").content(state.map(|s|s.scripts.template.as_ref().map(|t|t.snippet.name.clone()).unwrap_or_default())),crate::help_ui::tip("Check targets",state.map(|s|if s.scripts.template.as_ref().is_some_and(|t|t.configuration.is_some()) {"Preflight uploads a private candidate and saves the original on each server. Review the session-only diff before publishing. The live file is unchanged until execution."} else if s.scripts.template.as_ref().is_some_and(|t|t.recovery.is_some()) {"Preflight freezes a private recovery plan without copying or releasing files. Review residuals and destination. Release verifies exported checksums on the server before deletion."} else if s.scripts.template.as_ref().is_some_and(|t|t.cleanup.is_some()||t.audit_cleanup.is_some()) {"Preflight rechecks candidates and freezes a private server plan. It does not remove records. Review every exact file plan before submitting irreversible server cleanup."} else {"Preflight is read-only. Review every target plan before submitting server jobs. Root or sudo and systemd-run are required."}.to_owned())))),
            text().class("script-code").selectable(true).content(state.map(|s|s.scripts.template.as_ref().map(|t|t.drill.as_ref().and_then(|j|j.review().ok()).unwrap_or_else(||t.snippet.command.clone())).unwrap_or_default())),
            col().class("form-stack").fill((
            text().class("installation-scope hint").content(state.map(|s|s.scripts.template.as_ref().and_then(|t|t.installation.as_ref()).map(|_|i18n::translate("Failed installation can leave partial changes. Review results before retrying; no automatic rollback or removal.")).unwrap_or_default())),
            crate::installation_ui::options(ui.clone()),
            )),
            when(state.map(|s|s.scripts.template.as_ref().is_some_and(|t|t.configuration.is_some())),||text().class("hint").content(i18n::bind("Check targets reads the file on the server and shows the changes here. Nothing changes on the server until you run the task."))),
            when(state.map(|s|s.scripts.template.as_ref().is_some_and(|t|t.service.is_some())),||text().class("hint").content(i18n::bind("Service changes may interrupt connections. Check the unit on every target before running."))),
            when(state.map(|s|s.scripts.template.as_ref().is_some_and(|t|t.is_cross_server_recovery())),||text().class("hint cross-server-recovery-scope").content(i18n::bind("Cross-server recovery: trusted restore targets may be outside the original backup policy. Each target must access the same repository and prepared credential files. The source server is not contacted; backup and retention target scopes are unchanged."))),
            when(state.map(|s|s.scripts.template.as_ref().is_some_and(|t|t.cleanup.is_some()||t.audit_cleanup.is_some()||t.recovery.as_ref().is_some_and(|j|j.targets.iter().any(|target|target.plan.action==arcnix_core::record_recovery::Action::Release)))),||text().class("hint").content(i18n::bind("Deletion cannot be undone. Review the exact file plan before execution."))),
            col().class("form-stack").fill(crate::script_parameters_ui::run_form(ui.clone())),
            row().class("script-actions").fill((text().class("field-label").content(i18n::bind("Parallel submissions")),row().class("script-actions").fill(each(state.map(|s|if s.scripts.template.as_ref().and_then(|t|t.backup.as_ref()).is_some_and(|j|!matches!(j.action,arcnix_core::backups::Action::Backup)){vec![1usize]}else{vec![1usize,3,5,8]})).key(|n|*n).children(move|item|{let n=item.value.get();btn(format!("{n}"),move||state.revise(|s|s.scripts.concurrency=n)).class(state.map(move|s|if s.scripts.concurrency==n {"choice active"}else{"choice"}.to_owned()))})),crate::help_ui::group("Parallel submissions",&["Maximum parallel submissions; accepted jobs run independently on all servers","Each server runs one global Arcnix operation at a time. Server locks survive client closure."]))),
            task_target_picker::picker(ui.clone()),
            text().class("hint").content(i18n::bind("Parameters and script text must not contain passwords or tokens. Credentials are used only for this submission.")),
        )),(btn("Cancel",move||cancel.close_dialog()),btn("Check targets",move||prepare.prepare_script(Submission::ReviewBatch)).class("button primary").enabled(state.map(|s|task_target_picker::hidden(s)==0))))
            },
        ),
    ))
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[test]
    fn observations_replace_stale_codes_and_preserve_the_required_check_gate() -> anyhow::Result<()>
    {
        let mut task = Task {
            stops: None,
            drill: None,
            installation: None,
            configuration: None,
            configuration_targets: None,
            diagnostic: None,
            backup: None,
            id: "job-1".into(),
            name: "Script".into(),
            script_id: "mine".into(),
            revision: "1".into(),
            created: 0,
            targets: vec![tasks::Target {
                id: "peer".into(),
                name: "Peer".into(),
                endpoint: "user@host:22".into(),
                fingerprint: "pin".into(),
                status: Status::Succeeded,
                preview: String::new(),
                output: "stale output".into(),
                exit_code: Some(0),
                remote_directory: "/home/user/job-1".into(),
                result_read_at: None,
            }],
        };
        for (service, expected) in [
            ("active", Status::Running),
            ("deactivating", Status::Stopping),
            ("unavailable", Status::Unknown),
            ("failed", Status::Unknown),
            ("inactive", Status::Succeeded),
        ] {
            let observation = arcnix_core::remote_tasks::parse_observation(&format!(
                "ARCNIX_TASK_QUERY_V2\tjob-1\t{service}\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\t{service}\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tQVJDTklYX1RBU0tfUkVTVUxUX1YxCWpvYi0xCWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhCTAK\nARCNIX_TASK_OUTPUT\nfresh output"
            ))?;
            apply_refresh(
                &mut task,
                "peer",
                Ok(batch_worker::Snapshot {
                    observation,
                    received_at: Some(1_700_000_001),
                }),
            );
            assert_eq!(task.targets[0].result_read_at, Some(1_700_000_001));
            assert_eq!(task.targets[0].status, expected);
            assert_eq!(
                task.targets[0].exit_code,
                if expected == Status::Succeeded {
                    Some(0)
                } else {
                    None
                }
            );
            assert!(task.targets[0].output.starts_with("fresh output"));
            assert!(!task.targets[0].output.contains("stale output"));
        }
        task.installation = Some(arcnix_core::installers::Definition::official(
            "nginx",
            vec!["nginx".into()],
        )?);
        let completed = arcnix_core::remote_tasks::parse_observation(
            "ARCNIX_TASK_QUERY_V2\tjob-1\tinactive\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tinactive\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tQVJDTklYX1RBU0tfUkVTVUxUX1YxCWpvYi0xCWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhCTAK\nARCNIX_TASK_OUTPUT\nMissing the required installation report",
        )?;
        apply_refresh(
            &mut task,
            "peer",
            Ok(batch_worker::Snapshot {
                observation: completed,
                received_at: Some(1_700_000_002),
            }),
        );
        assert_eq!(task.targets[0].status, Status::Unknown);
        assert_eq!(task.targets[0].exit_code, Some(0));
        assert_eq!(task.targets[0].result_read_at, Some(1_700_000_002));
        apply_refresh(&mut task, "peer", Err("Wrong task response".into()));
        assert_eq!(task.targets[0].result_read_at, Some(1_700_000_002));
        assert_eq!(task.targets[0].status, Status::Unknown);
        assert_eq!(task.targets[0].exit_code, None);
        Ok(())
    }
}
