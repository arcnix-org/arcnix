use arcnix_core::software_catalog::{self as catalog, App, Category, Inventory};
use arcnix_core::software_install::{self, Source};

use crate::i18n::desktop as i18n;
use crate::i18n::desktop::{btn, icon_action, text_action};
use crate::*;

#[derive(Clone, Default)]
pub(crate) struct AppCenterState {
    pub view: u8,
    pub query: String,
    pub category: Option<Category>,
    pub selected: Option<&'static str>,
    pub compose_only: bool,
    pub inventory: Option<Inventory>,
    pub pending: Option<u64>,
    pub error: String,
    pub all_packages: bool,
}
#[derive(Clone)]
enum Item {
    Package(Source, arcnix_core::packages::Package),
    Project(arcnix_core::compose_apps::Project),
    Container(arcnix_core::docker::Container),
}
impl Item {
    fn key(&self) -> String {
        if let Self::Container(container) = self {
            return format!("container:{}", container.id);
        }
        format!("{}:{}", self.source(), self.name())
    }
    fn name(&self) -> String {
        match self {
            Self::Package(_, p) => p.name.clone(),
            Self::Project(p) => p.definition.name().to_owned(),
            Self::Container(c) => c.name.clone(),
        }
    }
    fn app(&self) -> Option<App> {
        match self {
            Self::Package(source, p) => catalog::for_package(*source, &p.name),
            Self::Project(p) => catalog::APPS
                .iter()
                .find(|a| a.compose.is_some() && a.compose == p.definition.kind())
                .copied(),
            Self::Container(c) => catalog::for_container(c),
        }
    }
    fn source(&self) -> String {
        match self {
            Self::Package(Source::Native(m), _) => m.label().into(),
            Self::Package(Source::Snap, _) => "Snap".into(),
            Self::Project(_) => "Compose".into(),
            Self::Container(c) if !c.compose_project.is_empty() => "Docker Compose".into(),
            Self::Container(_) => "Docker".into(),
        }
    }
    fn version(&self) -> String {
        match self {
            Self::Package(_, p) => p.version.clone(),
            Self::Project(p) => p.definition.image().to_owned(),
            Self::Container(c) => format!("{} · {}", c.image, c.status),
        }
    }
}
fn items(s: &State) -> Vec<Item> {
    let Some(inv) = &s.software.app_center.inventory else {
        return vec![];
    };
    let source = Source::Native(inv.manager);
    inv.native
        .iter()
        .filter(|p| {
            s.software.app_center.all_packages || catalog::for_package(source, &p.name).is_some()
        })
        .cloned()
        .map(|p| Item::Package(source, p))
        .chain(
            inv.snap
                .iter()
                .cloned()
                .map(|p| Item::Package(Source::Snap, p)),
        )
        .chain(inv.projects.iter().cloned().map(Item::Project))
        .chain(inv.containers.iter().cloned().map(Item::Container))
        .filter(|i| {
            format!(
                "{} {} {}",
                i.name(),
                i.source(),
                i.app().map(|a| a.name).unwrap_or_default()
            )
            .to_lowercase()
            .contains(&s.software.app_center.query.to_lowercase())
        })
        .collect()
}
/// Whether an application fits the search and the Compose filter.
fn matches_search(s: &State, app: App) -> bool {
    let app_center = &s.software.app_center;
    offered(s, app)
        && (!app_center.compose_only || app.compose.is_some())
        && format!(
            "{} {} {} {} {}",
            app.name,
            app.summary,
            app.uses,
            i18n::translate(app.summary),
            i18n::translate(app.category.label())
        )
        .to_lowercase()
        .contains(&app_center.query.to_lowercase())
}
/// Searched applications in `category`, or in every category.
fn in_category(s: &State, category: Option<Category>) -> Vec<App> {
    catalog::DISCOVERABLE
        .iter()
        .filter_map(|id| catalog::get(id))
        .filter(|a| category.is_none_or(|c| a.category == c) && matches_search(s, *a))
        .collect()
}
fn apps(s: &State) -> Vec<App> {
    in_category(s, s.software.app_center.category)
}
fn installed(s: &State, app: App) -> bool {
    s.software.app_center.inventory.as_ref().is_some_and(|i| {
        i.native
            .iter()
            .any(|p| app.matches_package(i.manager, &p.name))
            || app.id == "docker" && i.docker_cli
            || i.snap
                .iter()
                .any(|p| app.snap.is_some_and(|v| v.name == p.name))
            || i.projects
                .iter()
                .any(|p| app.compose.is_some() && app.compose == p.definition.kind())
            || i.containers
                .iter()
                .any(|c| catalog::for_container(c).is_some_and(|a| a.id == app.id))
    })
}
fn badge(app: App) -> impl Widget {
    text()
        .class(format!(
            "app-center-app-icon app-center-color-{}",
            app.color
        ))
        .content(app.mark)
}

/// Where an application stands on this server, or on this computer for
/// applications that run inside Arcnix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Checking,
    Installing,
    Unknown,
    Installed,
    Missing,
    Unavailable,
}
impl Status {
    fn label(self, app: App) -> &'static str {
        match (self, app.local) {
            (Self::Installing, _) => "Installing pgAdmin…",
            (Self::Checking, _) => "Checking…",
            (Self::Installed, true) => "Ready on this computer",
            (Self::Unavailable, _) => "No available source",
            (_, true) => "Downloads when first opened",
            (Self::Installed, false) => "Installed",
            (Self::Missing, false) => "Not installed",
            (Self::Unknown, false) => "Status unavailable",
        }
    }
    fn class(self) -> &'static str {
        match self {
            Self::Installed => "app-center-status installed",
            Self::Unavailable => "app-center-status unavailable",
            _ => "app-center-status",
        }
    }
}
fn status(s: &State, app: App) -> Status {
    let app_center = &s.software.app_center;
    if app.local {
        return if s.pgadmin_component.installing() {
            Status::Installing
        } else if matches!(s.pgadmin_component, pgadmin_ui::ComponentState::Ready) {
            Status::Installed
        } else if arcnix_core::pgadmin::supported_platform()
            || arcnix_core::pgadmin::local_package().is_some()
        {
            Status::Missing
        } else {
            Status::Unavailable
        };
    }
    match &app_center.inventory {
        None if app_center.pending.is_some() => Status::Checking,
        None => Status::Unknown,
        Some(_) if installed(s, app) => Status::Installed,
        Some(_)
            if (0..3)
                .any(|source| software_install_ui::catalog_source_available(s, app, source)) =>
        {
            Status::Missing
        }
        Some(_) => Status::Unavailable,
    }
}
/// The Arcnix window that works with this application, when it is offered here.
fn window(s: &State, app: App) -> Option<WorkspacePage> {
    let page = match app.id {
        "pgadmin" => WorkspacePage::PgAdmin,
        "docker" => WorkspacePage::Docker,
        "nginx" => WorkspacePage::Nginx,
        "redis" => WorkspacePage::Redis,
        _ => return None,
    };
    (s.connected && server_desktop::available_apps(s).contains(&page)).then_some(page)
}
/// Local applications are listed only where their window is offered.
fn offered(s: &State, app: App) -> bool {
    !app.local || app.id == "pgadmin" && pgadmin_available(s)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Primary {
    Install,
    Open(WorkspacePage),
    Manage,
}
fn primary(s: &State, app: App) -> Primary {
    match (status(s, app), window(s, app)) {
        (_, Some(page)) if app.local => Primary::Open(page),
        (Status::Installed, Some(page)) => Primary::Open(page),
        (Status::Installed, None) => Primary::Manage,
        _ if app.local => Primary::Manage,
        _ => Primary::Install,
    }
}
fn installations(s: &State, app: App) -> Vec<Item> {
    let Some(inv) = &s.software.app_center.inventory else {
        return vec![];
    };
    let source = Source::Native(inv.manager);
    inv.native
        .iter()
        .filter(|p| app.matches_package(inv.manager, &p.name))
        .cloned()
        .map(|p| Item::Package(source, p))
        .chain(
            inv.snap
                .iter()
                .filter(|p| app.snap.is_some_and(|snap| snap.name == p.name))
                .cloned()
                .map(|p| Item::Package(Source::Snap, p)),
        )
        .chain(
            inv.projects
                .iter()
                .filter(|p| app.compose.is_some() && app.compose == p.definition.kind())
                .cloned()
                .map(Item::Project),
        )
        .chain(
            inv.containers
                .iter()
                .filter(|c| catalog::for_container(c).is_some_and(|a| a.id == app.id))
                .cloned()
                .map(Item::Container),
        )
        .collect()
}

impl Ui {
    pub(crate) fn open_app_center(&self, compose_only: bool) {
        self.state.revise(|s| {
            s.software.app_center.view = 0;
            s.software.app_center.selected = None;
            s.software.app_center.query.clear();
            s.software.app_center.category = None;
            s.software.app_center.compose_only = compose_only;
        });
        self.open_desktop_app(WorkspacePage::AppCenter);
        self.app_center_enter();
    }
    /// A quick check of local files, repeated whenever App Center is shown.
    pub(crate) fn app_center_local_status(&self) {
        if let Ok(base) = arcnix_core::pgadmin::base(&self.store) {
            self.refresh_pgadmin_component(&base);
        }
    }
    pub(crate) fn app_center_enter(&self) {
        if self
            .state
            .with(|s| s.workspace_page == WorkspacePage::AppCenter)
        {
            self.app_center_local_status();
        }
        if self.state.with(|s| {
            s.workspace_page == WorkspacePage::AppCenter
                && s.software.app_center.inventory.is_none()
                && s.software.app_center.pending.is_none()
                && s.software.app_center.error.is_empty()
        }) {
            self.app_center_refresh();
        }
    }
    pub(crate) fn app_center_refresh(&self) {
        self.app_center_local_status();
        if !self
            .state
            .with(|s| s.connected && s.software.app_center.pending.is_none())
        {
            return;
        }
        let serial = self.send(Action::ServerApps(
            server_apps_worker::Action::SoftwareInventory,
        ));
        self.state.revise(|s| {
            s.software.app_center.pending = Some(serial);
            s.software.app_center.error.clear();
        });
    }
    fn app_center_details(&self, id: &'static str) {
        self.state.revise(|s| {
            s.software.app_center.selected = Some(id);
            s.software.app_center.view = 2;
        });
    }
    fn app_center_item_action(&self, item: Item, action: software_install::Action) {
        let Item::Package(source, p) = item else {
            return;
        };
        self.open_software_action(source, action, vec![p.name]);
    }
    pub(crate) fn refresh_software_inventory(&self) {
        self.state.revise(|s| {
            s.server_apps.discover_pending = None;
            s.software.app_center.pending = None;
        });
        self.discover_server_apps();
        self.app_center_refresh();
        if self.state.with(|s| s.docker.view == 3) {
            self.compose_refresh();
        }
        if self.state.with(|s| s.software.app_center.view == 3) {
            if self.state.with(|s| s.software.installed_view) {
                self.software_installed();
            } else {
                self.software_search();
            }
        }
    }
}
pub(crate) fn event(
    ui: &Ui,
    serial: u64,
    result: std::result::Result<server_apps_worker::Reply, String>,
) -> Option<std::result::Result<server_apps_worker::Reply, String>> {
    if !ui
        .state
        .with(|s| s.software.app_center.pending == Some(serial))
    {
        return Some(result);
    }
    ui.state.revise(|s| {
        s.software.app_center.pending = None;
        match result {
            Ok(server_apps_worker::Reply::SoftwareInventory(inv)) => {
                s.software.app_center.error = inv.errors.join("\n");
                s.software.app_center.inventory = Some(inv)
            }
            Err(e) => s.software.app_center.error = e,
            _ => s.software.app_center.error = "Invalid software inventory response".into(),
        }
    });
    None
}
fn primary_button(ui: Ui, app: App, class: &'static str) -> sapid::Button {
    let state = ui.state;
    sapid::button()
        .class(class)
        .label(i18n::bind(state.map(move |s| {
            match primary(s, app) {
                Primary::Install => "Install…",
                Primary::Open(_) => "Open",
                Primary::Manage if app.local => "Details",
                Primary::Manage => "Manage",
            }
            .to_owned()
        })))
        .enabled(state.map(move |s| match primary(s, app) {
            Primary::Install => can_install(s, app),
            _ => true,
        }))
        .on_press(move || match ui.state.with(|s| primary(s, app)) {
            Primary::Install => ui.install_catalog_app(app.id),
            Primary::Open(page) => ui.open_desktop_app(page),
            Primary::Manage => ui.app_center_details(app.id),
        })
}
fn status_text(state: Cage<State>, app: App, card: bool) -> impl Widget {
    text()
        .class(state.map(move |s| {
            let status = status(s, app);
            // Cards only point out what changes the button next to them.
            let hidden = card
                && !matches!(
                    status,
                    Status::Installed | Status::Installing | Status::Unavailable
                );
            format!("{}{}", status.class(), if hidden { " hidden" } else { "" })
        }))
        .content(i18n::bind(
            state.map(move |s| status(s, app).label(app).to_owned()),
        ))
}
fn card(ui: Ui, app: App) -> impl Widget {
    let state = ui.state;
    let detail = ui.clone();
    col().class("app-center-app-card").fill((
        sapid::button()
            .class("app-center-card-open")
            .name(app.name)
            .label("")
            .fill((
                badge(app),
                text().class("app-center-card-name").content(app.name),
                text()
                    .class("app-center-card-category subtle")
                    .content(i18n::bind(app.category.label())),
                text()
                    .class("app-center-card-summary")
                    .content(i18n::bind(app.summary)),
            ))
            .on_press(move || detail.app_center_details(app.id)),
        row().class("app-center-card-footer").fill((
            status_text(state, app, true),
            primary_button(ui, app, "button app-center-install-button"),
        )),
    ))
}
fn can_install(s: &State, app: App) -> bool {
    !app.local
        && software_install_ui::can_change_software(s)
        && (0..3).any(|source| software_install_ui::catalog_source_available(s, app, source))
}
fn categories(state: Cage<State>) -> impl Widget {
    col()
        .class("app-center-categories")
        .name(i18n::bind("Application categories"))
        .fill(
            std::iter::once(None)
                .chain(Category::ALL.into_iter().map(Some))
                .map(move |category| {
                    let label = category.map_or("All applications", Category::label);
                    // A page shows where its application is filed.
                    let pressed = move |s: &State| {
                        let app_center = &s.software.app_center;
                        if app_center.view == 2 {
                            app_center
                                .selected
                                .and_then(catalog::get)
                                .map(|a| a.category)
                                == category
                        } else {
                            app_center.category == category
                        }
                    };
                    sapid::button()
                        .class(state.map(move |s| {
                            // Categories without a matching application are left out.
                            if category.is_some()
                                && !pressed(s)
                                && in_category(s, category).is_empty()
                            {
                                "button compact hidden"
                            } else {
                                "button compact"
                            }
                            .to_owned()
                        }))
                        .name(i18n::bind(label))
                        .fill((
                            text()
                                .class("app-center-category-label")
                                .content(i18n::bind(label)),
                            text().class("app-center-category-count").content(
                                state.map(move |s| in_category(s, category).len().to_string()),
                            ),
                        ))
                        .pressed(state.map(pressed))
                        .on_press(move || {
                            state.revise(|s| {
                                s.software.app_center.category = category;
                                s.software.app_center.view = 0;
                            })
                        })
                })
                .collect::<Vec<_>>(),
        )
}

fn discover(ui: Ui) -> impl Widget {
    let state = ui.state;
    let all = ui.clone();
    col().class("app-center-discover").fill((
        when(
            state.map(|s| s.software.app_center.compose_only),
            move || {
                row().class("app-center-context-filter").fill((
                    text().content(i18n::bind("Compose-compatible applications")),
                    text_action("Show all applications", Icon::Clear).on_press({
                        let all = all.clone();
                        move || {
                            all.state
                                .revise(|s| s.software.app_center.compose_only = false)
                        }
                    }),
                ))
            },
        ),
        row().class("app-center-section-heading").fill((
            text()
                .class("section-title")
                .content(i18n::bind(state.map(|s| {
                    s.software
                        .app_center
                        .category
                        .map_or("Explore applications", Category::label)
                        .to_owned()
                }))),
            text()
                .class("app-center-count")
                .content(state.map(|s| apps(s).len().to_string())),
        )),
        when(
            state.map(|s| s.software.app_center.pending.is_some()),
            || {
                text()
                    .class("subtle")
                    .content(i18n::bind("Checking available installation sources…"))
            },
        ),
        col().class("app-center-app-grid").fill(
            each(state.map(apps))
                .key(|a| a.id)
                .children(move |item| card(ui.clone(), item.value.get())),
        ),
        when(state.map(|s| apps(s).is_empty()), || {
            text()
                .class("app-center-empty")
                .content(i18n::bind("No matching applications"))
        }),
    ))
}
fn nginx_window(s: &State) -> bool {
    s.connected && server_desktop::available_apps(s).contains(&WorkspacePage::Nginx)
}
fn redis_window(s: &State) -> bool {
    s.connected && server_desktop::available_apps(s).contains(&WorkspacePage::Redis)
}
fn pgadmin_available(s: &State) -> bool {
    server_desktop::available_apps(s).contains(&WorkspacePage::PgAdmin)
}
/// Management actions for one installation, shared by Installed and app pages.
fn item_actions(ui: Ui, value: Cage<Item>) -> impl Widget {
    let state = ui.state;
    let service = ui.clone();
    let docker = ui.clone();
    let upgrade = ui.clone();
    let remove = ui.clone();
    let manage = ui.clone();
    let container_ui = ui.clone();
    fragment((
        when(value.map(|i| matches!(i, Item::Package(..))), move || {
            row().class("app-center-installed-actions").fill((
                when(
                    value.map(|i| {
                        matches!(i, Item::Package(Source::Native(_), _))
                            && i.app().is_some_and(|app| app.id == "nginx")
                    }),
                    {
                        let service = service.clone();
                        move || {
                            let service = service.clone();
                            // The Nginx app manages the service once it has
                            // been detected; until then, Monitor does.
                            sapid::button()
                                .class("button app-center-open")
                                .label(i18n::bind(state.map(|s| {
                                    if nginx_window(s) {
                                        "Open Nginx"
                                    } else {
                                        "Manage service"
                                    }
                                    .to_owned()
                                })))
                                .on_press(move || {
                                    if service.state.with(nginx_window) {
                                        service.open_desktop_app(WorkspacePage::Nginx);
                                    } else {
                                        service.open_system_service("nginx.service");
                                    }
                                })
                        }
                    },
                ),
                when(
                    value.map(|i| {
                        matches!(i, Item::Package(Source::Native(_), _))
                            && i.app().is_some_and(|app| app.id == "redis")
                    }),
                    {
                        let service = service.clone();
                        move || {
                            let service = service.clone();
                            sapid::button()
                                .class("button app-center-open")
                                .label(i18n::bind(state.map(|s| {
                                    if redis_window(s) {
                                        "Open Redis"
                                    } else {
                                        "Manage service"
                                    }
                                    .to_owned()
                                })))
                                .on_press(move || {
                                    if service.state.with(redis_window) {
                                        service.open_desktop_app(WorkspacePage::Redis);
                                    } else {
                                        service.open_system_service("redis-server.service");
                                    }
                                })
                        }
                    },
                ),
                when(
                    state.map(move |s| {
                        s.server_apps.applications.docker
                            && value.get().app().is_some_and(|app| app.id == "docker")
                    }),
                    {
                        let docker = docker.clone();
                        move || {
                            let docker = docker.clone();
                            btn("Open Docker", move || {
                                docker.open_desktop_app(WorkspacePage::Docker)
                            })
                            .class("button app-center-open")
                        }
                    },
                ),
                btn("Upgrade…", {
                    let upgrade = upgrade.clone();
                    move || {
                        upgrade
                            .app_center_item_action(value.get(), software_install::Action::Upgrade)
                    }
                })
                .enabled(state.map(software_install_ui::can_change_software)),
                btn("Remove…", {
                    let remove = remove.clone();
                    move || {
                        remove.app_center_item_action(value.get(), software_install::Action::Remove)
                    }
                })
                .class("button app-center-remove")
                .enabled(state.map(software_install_ui::can_change_software)),
            ))
        }),
        when(value.map(|i| matches!(i, Item::Project(..))), move || {
            row().class("app-center-installed-actions").fill(
                btn("Manage project", {
                    let manage = manage.clone();
                    move || {
                        if let Item::Project(project) = value.get() {
                            manage.open_compose_project(project.definition.name().to_owned());
                        }
                    }
                })
                .class("button app-center-open"),
            )
        }),
        when(value.map(|i| matches!(i, Item::Container(..))), move || {
            let container_ui = container_ui.clone();
            row().class("app-center-installed-actions").fill(
                btn("Open container", move || {
                    if let Item::Container(container) = value.get() {
                        container_ui.open_docker_container(container.id);
                    }
                })
                .class("button app-center-open")
                .enabled(state.map(docker_ui::ready)),
            )
        }),
    ))
}
fn section(title: &'static str, body: impl Widget) -> impl Widget {
    col().class("app-center-detail-section").fill((
        text().class("section-title").content(i18n::bind(title)),
        body,
    ))
}
/// Each installation of a server application, with what can be done to it.
fn server_installations(ui: Ui, app: App) -> impl Widget {
    let state = ui.state;
    col().class("app-center-installations").fill((
        each(state.map(move |s| installations(s, app)))
            .key(Item::key)
            .children(move |item| {
                let value = item.value;
                row().class("app-center-installation").fill((
                    text()
                        .class("app-center-source-badge")
                        .content(value.map(Item::source)),
                    col().class("app-center-installation-name").fill((
                        text()
                            .class("app-center-package-name")
                            .selectable(true)
                            .content(value.map(Item::name)),
                        text()
                            .class("app-center-installed-version subtle")
                            .content(value.map(Item::version)),
                    )),
                    item_actions(ui.clone(), value),
                ))
            }),
        // Docker can also come from Docker's own installation script.
        when(state.map(move |s| installations(s, app).is_empty()), || {
            text().class("subtle").content(i18n::bind(
                "Found on this server outside its package manager.",
            ))
        }),
    ))
}
/// Whether this source already provides an installation of `app`.
pub(crate) fn source_in_use(s: &State, app: App, source: u8) -> bool {
    installations(s, app).iter().any(|item| {
        matches!(
            (source, item),
            (0, Item::Package(Source::Native(_), _))
                | (1, Item::Package(Source::Snap, _))
                | (2, Item::Project(_))
        )
    })
}
fn source_state(s: &State, app: App, source: u8) -> (&'static str, &'static str) {
    if source_in_use(s, app, source) {
        ("Installed", " installed")
    } else if software_install_ui::catalog_source_available(s, app, source) {
        ("Available", " available")
    } else {
        ("Unavailable", "")
    }
}
fn install_sources(state: Cage<State>, app: App) -> impl Widget {
    col().class("app-center-sources").fill(
        [
            (0, "System packages", Icon::Server),
            (1, "Snap", Icon::Grid),
            (2, "Docker Compose", Icon::Docker),
        ]
        .into_iter()
        .map(move |(source, label, icon)| {
            row()
                .class(
                    state.map(move |s| {
                        format!("app-center-source{}", source_state(s, app, source).1)
                    }),
                )
                .fill((
                    icons::glyph(icon),
                    col().class("app-center-source-text").fill((
                        text()
                            .class("app-center-source-name")
                            .content(i18n::bind(label)),
                        text()
                            .class("app-center-source-note subtle")
                            .content(i18n::bind(state.map(move |s| {
                                software_install_ui::source_note(s, Some(app), source)
                            }))),
                    )),
                    text().class("app-center-source-state").content(i18n::bind(
                        state.map(move |s| source_state(s, app, source).0.to_owned()),
                    )),
                ))
        })
        .collect::<Vec<_>>(),
    )
}
/// The pgAdmin component: where it runs and whether it is downloaded.
fn local_installation(state: Cage<State>, app: App) -> impl Widget {
    let size = arcnix_core::pgadmin::package().map(|asset| formatting::bytes(asset.size));
    row().class("app-center-installation").fill((
        text()
            .class("app-center-source-badge")
            .content(i18n::bind("This computer")),
        col().class("app-center-installation-name").fill((
            text().content(format!("{} {}", app.name, arcnix_core::pgadmin::VERSION)),
            text()
                .class("app-center-installed-version subtle")
                .content(i18n::bind(state.map(move |s| {
                    match (status(s, app), &size) {
                        (Status::Installing, _) => "Installing pgAdmin…".to_owned(),
                        (Status::Unavailable, _) => "No available source".to_owned(),
                        (Status::Installed, _) => "Downloaded".to_owned(),
                        (_, Some(size)) => format!("Not downloaded · {size}"),
                        _ => "Not downloaded".to_owned(),
                    }
                }))),
        )),
    ))
}
fn related(ui: Ui, app: App) -> impl Widget {
    let state = ui.state;
    col().class("app-center-related").fill(
        each(state.map(move |s| {
            app.related
                .iter()
                .filter_map(|id| catalog::get(id))
                .filter(|other| offered(s, *other))
                .collect::<Vec<_>>()
        }))
        .key(|a| a.id)
        .children(move |item| {
            let other = item.value.get();
            let open = ui.clone();
            row().class("app-center-related-card").fill((
                sapid::button()
                    .class("app-center-related-open")
                    .name(other.name)
                    .label("")
                    .fill((
                        text()
                            .class(format!(
                                "app-center-app-icon small app-center-color-{}",
                                other.color
                            ))
                            .content(other.mark),
                        text().class("app-center-entry-name").content(other.name),
                        text()
                            .class("app-center-entry-detail subtle")
                            .content(i18n::bind(other.summary)),
                    ))
                    .on_press(move || open.app_center_details(other.id)),
                status_text(state, other, false),
                primary_button(ui.clone(), other, "button app-center-install-button"),
            ))
        }),
    )
}
fn detail(ui: Ui) -> impl Widget {
    let state = ui.state;
    each(state.map(|s| {
        s.software
            .app_center
            .selected
            .and_then(catalog::get)
            .into_iter()
            .collect::<Vec<_>>()
    }))
    .key(|a| a.id)
    .children(move |item| {
        let app = item.value.get();
        let website = app.website;
        col().class("app-center-detail").fill((
            text_action("Back", Icon::ChevronRight)
                .class("button with-icon app-center-back")
                .on_press(move || state.revise(|s| s.software.app_center.view = 0)),
            row().class("app-center-detail-header").fill((
                badge(app),
                col().class("app-center-detail-identity").fill((
                    text().class("app-center-detail-name").content(app.name),
                    text()
                        .class("app-center-detail-summary")
                        .content(i18n::bind(app.summary)),
                    row().class("app-center-detail-meta").fill((
                        status_text(state, app, false),
                        text()
                            .class("subtle")
                            .content(i18n::bind(app.category.label())),
                    )),
                )),
                // Installed server applications are managed per installation below.
                when(
                    state.map(move |s| primary(s, app) != Primary::Manage),
                    {
                        let ui = ui.clone();
                        move || primary_button(ui.clone(), app, "button primary app-center-primary")
                    },
                ),
            )),
            when(Vary::Value(app.local), move || {
                section("On this computer", local_installation(state, app))
            }),
            when(
                state.map(move |s| !app.local && status(s, app) == Status::Installed),
                {
                    let ui = ui.clone();
                    move || section("On this server", server_installations(ui.clone(), app))
                },
            ),
            when(
                state.map(move |s| {
                    !app.local && !matches!(status(s, app), Status::Checking | Status::Unknown)
                }),
                {
                    let ui = ui.clone();
                    move || {
                        let install = ui.clone();
                        col().class("app-center-detail-section").fill((
                            row().class("section-heading").fill((
                                text().class("section-title").content(i18n::bind(state.map(
                                    move |s| {
                                        if status(s, app) == Status::Installed {
                                            "Other ways to install"
                                        } else {
                                            "Ways to install"
                                        }
                                        .to_owned()
                                    },
                                ))),
                                help_ui::tip("Application catalog","Built-in application descriptions are available offline. Installation uses the server's enabled repositories or the selected publisher's snap/image. Versions and package availability vary; package installation may require additional service initialization. No third-party repository is added automatically."),
                                // A second source is a separate installation, such as Compose next to a package.
                                when(
                                    state.map(move |s| status(s, app) == Status::Installed),
                                    move || {
                                        let install = install.clone();
                                        btn("Install another way…", move || {
                                            let used: Vec<u8> = install.state.with(|s| {
                                                (0..3)
                                                    .filter(|source| source_in_use(s, app, *source))
                                                    .collect()
                                            });
                                            install.install_catalog_app_avoiding(app.id, &used)
                                        })
                                        .class("button app-center-install-button")
                                        .enabled(state.map(move |s| can_install(s, app)))
                                    },
                                ),
                            )),
                            install_sources(state, app),
                        ))
                    }
                },
            ),
            section(
                "About",
                col().class("app-center-about").fill((
                    text()
                        .class("app-center-description")
                        .content(i18n::bind(app.description)),
                    row().class("app-center-uses").fill(
                        each(state.map(move |s| {
                            // Both catalogs separate uses the same way.
                            crate::i18n::translate_for(
                                app.uses,
                                desktop_system_settings::language(s),
                            )
                            .split(" · ")
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                        }))
                        .key(|used| used.clone())
                        .children(|item| text().class("app-center-use").content(item.value.get())),
                    ),
                    sapid::button()
                        .class("app-center-website")
                        .label(website)
                        .on_press(move || {
                            let _ = open_url(website);
                        }),
                )),
            ),
            when(
                state.map(move |s| {
                    app.related
                        .iter()
                        .filter_map(|id| catalog::get(id))
                        .any(|other| offered(s, other))
                }),
                {
                    let ui = ui.clone();
                    move || section("Related apps", related(ui.clone(), app))
                },
            ),
        ))
    })
}
fn installed_identity(ui: Ui, value: Cage<Item>) -> impl Widget {
    let initial = value.get();
    let app = initial.app();
    let content = (
        text()
            .class(format!(
                "app-center-app-icon small app-center-color-{}",
                app.map(|a| a.color).unwrap_or("teal")
            ))
            .content(app.map(|a| a.mark).unwrap_or("▦")),
        text().class("app-center-entry-name").content(
            app.map(|a| a.name.to_owned())
                .unwrap_or_else(|| initial.name()),
        ),
        text()
            .class("app-center-entry-detail subtle")
            .content(value.map(Item::name)),
    );
    // Catalog applications open their page; other packages have none.
    sapid::button()
        .class(if app.is_some() {
            "app-center-installed-open"
        } else {
            "app-center-installed-open plain"
        })
        .name(app.map_or_else(|| initial.name(), |a| a.name.to_owned()))
        .label("")
        .fill(content)
        .on_press(move || {
            if let Some(app) = app {
                ui.app_center_details(app.id);
            }
        })
}
fn installed_local_apps(s: &State) -> impl Iterator<Item = App> + '_ {
    catalog::APPS.iter().copied().filter(|app| {
        app.local
            && offered(s, *app)
            && status(s, *app) == Status::Installed
            && format!("{} {}", app.name, i18n::translate(app.summary))
                .to_lowercase()
                .contains(&s.software.app_center.query.to_lowercase())
    })
}

fn installed_page(ui: Ui) -> impl Widget {
    let state = ui.state;
    let refresh = ui.clone();
    let local = ui.clone();
    col().class("app-center-installed").fill((
        row().class("app-center-inventory-toolbar").fill((
            row().class("app-center-section-heading").fill((
                text()
                    .class("section-title")
                    .content(i18n::bind("Installed applications")),
                text().class("app-center-count").content(state.map(|s| {
                    if s.software.app_center.inventory.is_none() {
                        String::new()
                    } else {
                        (items(s).len() + installed_local_apps(s).count()).to_string()
                    }
                })),
            )),
            sapid::checkbox()
                .class("preference-check")
                .label(i18n::bind("Show all system packages"))
                .checked(state.map(|s| s.software.app_center.all_packages))
                .on_checked_change(move |v| {
                    state.revise(|s| s.software.app_center.all_packages = v)
                }),
            icon_action("Refresh installed applications", Icon::Refresh)
                .enabled(state.map(|s| s.connected && s.software.app_center.pending.is_none()))
                .on_press(move || refresh.app_center_refresh()),
        )),
        when(
            state.map(|s| s.software.app_center.pending.is_some()),
            move || text().class("subtle").content(i18n::bind("Loading…")),
        ),
        col().class("app-center-inventory-list").fill((
            each(state.map(items)).key(Item::key).children({
                let ui = ui.clone();
                move |item| {
                    let value = item.value;
                    row().class("app-center-installed-row").fill((
                        installed_identity(ui.clone(), value),
                        text()
                            .class("app-center-source-badge")
                            .content(value.map(Item::source)),
                        text()
                            .class("app-center-installed-version subtle")
                            .content(value.map(Item::version)),
                        item_actions(ui.clone(), value),
                    ))
                }
            }),
            // Applications that run on this computer are listed once downloaded.
            each(state.map(|s| installed_local_apps(s).collect::<Vec<_>>()))
                .key(|a| a.id)
                .children(move |item| {
                    let app = item.value.get();
                    let details = local.clone();
                    row().class("app-center-installed-row").fill((
                        sapid::button()
                            .class("app-center-installed-open")
                            .name(app.name)
                            .label("")
                            .fill((
                                text()
                                    .class(format!(
                                        "app-center-app-icon small app-center-color-{}",
                                        app.color
                                    ))
                                    .content(app.mark),
                                text().class("app-center-entry-name").content(app.name),
                                text()
                                    .class("app-center-entry-detail subtle")
                                    .content(i18n::bind(app.summary)),
                            ))
                            .on_press(move || details.app_center_details(app.id)),
                        text()
                            .class("app-center-source-badge")
                            .content(i18n::bind("This computer")),
                        text()
                            .class("app-center-installed-version subtle")
                            .content(arcnix_core::pgadmin::VERSION),
                        row()
                            .class("app-center-installed-actions")
                            .fill(primary_button(local.clone(), app, "button app-center-open")),
                    ))
                }),
        )),
        when(
            state.map(|s| {
                s.software.app_center.pending.is_none()
                    && s.software.app_center.inventory.is_some()
                    && items(s).is_empty()
                    && installed_local_apps(s).next().is_none()
            }),
            move || {
                text()
                    .class("app-center-empty")
                    .content(i18n::bind("No matching installed applications"))
            },
        ),
    ))
}
pub(crate) fn page(ui: Ui) -> impl Widget {
    let state = ui.state;
    let custom = ui.clone();
    let refresh = ui.clone();
    let search = ui.clone();
    let catalog_ui = ui.clone();
    let installed_ui = ui.clone();
    let packages_ui = ui.clone();
    col().class("app-center-content").fill((
        row().class("app-center-toolbar").fill((
            row().class("app-center-navigation").fill(
                [(0, "Discover"), (1, "Installed")]
                    .into_iter()
                    .map(move |(view, label)| {
                        sapid::button()
                            .class("button app-center-nav-button")
                            .label(i18n::bind(label))
                            .pressed(state.map(move |s| {
                                s.software.app_center.view == view
                                    || view == 0 && s.software.app_center.view == 2
                            }))
                            .on_press(move || state.revise(|s| s.software.app_center.view = view))
                    })
                    .collect::<Vec<_>>(),
            ),
            input()
                .class("app-center-search")
                .name(i18n::bind("Search applications"))
                .semantics(state.map(|_| {
                    Some(Box::new(Semantics {
                        placeholder: Some(i18n::translate("Search applications")),
                        ..Default::default()
                    }))
                }))
                .value(state.map(|s| s.software.app_center.query.clone()))
                .on(value_changed, move |e| {
                    if let Event::ValueChanged { value, .. } = e {
                        state.revise(|s| {
                            s.software.app_center.query = value;
                            if s.software.app_center.view == 2 {
                                s.software.app_center.view = 0;
                            }
                        })
                    }
                }),
            text_action("Install by name…", Icon::Plus)
                .class("button with-icon app-center-custom-install")
                .enabled(state.map(|s| {
                    software_install_ui::can_change_software(s)
                        && s.software
                            .app_center
                            .inventory
                            .as_ref()
                            .is_some_and(|inv| software_install::supported(inv.manager) || s.server_apps.applications.snap || inv.compose_ready)
                }))
                .on_press(move || custom.open_software_install(0, Some(String::new()))),
            icon_action("Search package repositories", Icon::Search)
                .class("icon-button app-center-repository-search")
                .enabled(state.map(|s| s.connected))
                .on_press(move || {
                    state.revise(|s| {
                        s.software.app_center.view = 3;
                        s.software.source = 0;
                    });
                    search.software_installed();
                }),
            icon_action("Refresh installed applications", Icon::Refresh)
                .enabled(state.map(|s| s.connected && s.software.app_center.pending.is_none()))
                .on_press(move || refresh.app_center_refresh()),
        )),
        text()
            .class("error app-center-error")
            .content(state.map(|s| s.software.app_center.error.clone())),
        when(state.map(|s| s.software.app_center.inventory.as_ref().is_some_and(|inv| software_install::supported(inv.manager) && !inv.manager.uses_native_terminal() && !inv.is_root && !inv.sudo_available)), || {
            text().class("subtle").content(i18n::bind("To change system software, connect with an account that can use sudo or log in as root."))
        }),
        when(
            state.map(|s| matches!(s.software.app_center.view, 0 | 2)),
            move || {
                let discover_ui = catalog_ui.clone();
                let detail_ui = catalog_ui.clone();
                row().class("app-center-catalog").fill((
                    categories(state),
                    when(state.map(|s| s.software.app_center.view == 0), move || {
                        discover(discover_ui.clone())
                    })
                    .otherwise(move || detail(detail_ui.clone())),
                ))
            },
        ),
        when(state.map(|s| s.software.app_center.view == 1), move || {
            installed_page(installed_ui.clone())
        }),
        when(state.map(|s| s.software.app_center.view == 3), move || {
            software::packages(packages_ui.clone())
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        State::new(AppConfig::default(), String::new(), String::new())
    }

    #[test]
    fn local_apps_report_their_component_and_are_never_installed_on_the_server() {
        let pgadmin = catalog::get("pgadmin").unwrap();
        let mut s = state();
        let downloadable = arcnix_core::pgadmin::supported_platform()
            || arcnix_core::pgadmin::local_package().is_some();
        assert!(
            status(&s, pgadmin)
                == if downloadable {
                    Status::Missing
                } else {
                    Status::Unavailable
                }
        );
        s.pgadmin_component = pgadmin_ui::ComponentState::Ready;
        assert!(status(&s, pgadmin) == Status::Installed);
        assert!(!can_install(&s, pgadmin));
        assert!(installations(&s, pgadmin).is_empty());
        // Without a connection there is no window to open, so the card leads to its page.
        assert!(primary(&s, pgadmin) == Primary::Manage);
    }

    #[test]
    fn category_counts_follow_the_search() {
        let mut s = state();
        let all = in_category(&s, None).len();
        let counted: usize = Category::ALL
            .into_iter()
            .map(|c| in_category(&s, Some(c)).len())
            .sum();
        assert_eq!(counted, all);
        assert!(in_category(&s, Some(Category::Backup)).is_empty());
        s.software.app_center.query = "redis".into();
        assert_eq!(
            in_category(&s, None)
                .iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            ["redis"]
        );
        assert!(in_category(&s, Some(Category::Web)).is_empty());
    }

    #[test]
    fn server_apps_wait_for_the_inventory_before_offering_installation() {
        let postgresql = catalog::get("postgresql").unwrap();
        let mut s = state();
        assert!(status(&s, postgresql) == Status::Unknown);
        s.software.app_center.pending = Some(1);
        assert!(status(&s, postgresql) == Status::Checking);
        assert!(primary(&s, postgresql) == Primary::Install);
        assert!(!can_install(&s, postgresql));
        assert!(installations(&s, postgresql).is_empty());
    }
}
