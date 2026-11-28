use crate::*;

pub(crate) fn transfer_files(ui: Ui) -> impl Widget {
    col().class("files-workspace").fill((
        file_context::task_status(ui.clone()),
        splitter::panes(
            ui.clone(),
            pane(ui.clone(), false, false),
            pane(ui, true, false),
        ),
    ))
}

pub(crate) fn files(ui: Ui, window: u32) -> impl Widget {
    let state = ui.state;
    let live = state.map(move |s| s.workbench.files.is_live(window));
    col().class("remote-files").fill((
        when(live, {
            let ui = ui.clone();
            move || {
                fragment((
                    file_context::desktop_menu_dismiss(ui.clone()),
                    files_ui::drag_bridge(ui.clone()),
                    files_transfer::bridge(ui.clone()),
                    file_context::task_status(ui.clone()),
                ))
            }
        }),
        splitter::files_body(
            ui.clone(),
            splitter::FileSidebar::Files,
            {
                let ui = ui.clone();
                move || files_ui::sidebar(ui.clone(), window)
            },
            col().class("files-content-pane").fill((
                files_ui::tag_results(ui.clone(), window),
                when(state.map(move |s| files_tabs::showing_apps(s, window)), {
                    let ui = ui.clone();
                    move || desktop_apps::files(ui.clone(), window)
                }),
                when(live, move || {
                    fragment((
                        when(
                            state.map(|s| {
                                s.workbench.files.location == files_tabs::Location::Directory
                                    && s.server_apps.files_tag.is_none()
                            }),
                            {
                                let ui = ui.clone();
                                move || files_tab_tools(ui.clone())
                            },
                        ),
                        when(state.map(files_tabs::trash_active), {
                            let ui = ui.clone();
                            move || trash_ui::panel(ui.clone(), true, true)
                        }),
                        col()
                            .class(state.map(|s| {
                                if s.workbench.files.location != files_tabs::Location::Directory
                                    || s.server_apps.files_tag.is_some()
                                {
                                    "remote-slot files-slot hidden"
                                } else {
                                    "remote-slot files-slot"
                                }
                                .to_owned()
                            }))
                            .fill(pane(ui.clone(), true, true)),
                    ))
                })
                .otherwise(move || {
                    when(
                        state.map(move |s| {
                            !files_tabs::showing_apps(s, window)
                                && files_tabs::showing_tag(s, window).is_none()
                        }),
                        move || {
                            fragment((
                                files_tabs::preview_header(state, window),
                                files_tabs::preview_body(state, window),
                            ))
                        },
                    )
                }),
            )),
        ),
    ))
}

fn files_tab_tools(ui: Ui) -> impl Widget {
    row()
        .class("files-window-tools")
        .fill(file_browser::pathbar(
            ui.clone(),
            true,
            true,
            row().class("files-path-actions").fill((
                file_browser::view_controls(ui.clone(), true),
                pane_actions(ui, true, true),
            )),
        ))
}

pub(crate) fn ready(s: &State, remote: bool) -> bool {
    if remote {
        s.connected && s.sftp_available && s.remote_ready && !files_tabs::apps_active(s)
    } else {
        s.local_ready
    }
}
fn selected(s: &State, remote: bool) -> &HashSet<String> {
    if remote {
        &s.remote_selection
    } else {
        &s.local_selection
    }
}
fn selection(s: &mut State, remote: bool) -> &mut HashSet<String> {
    if remote {
        &mut s.remote_selection
    } else {
        &mut s.local_selection
    }
}
fn sort_button(
    ui: Ui,
    remote: bool,
    sort: Sort,
    desktop: bool,
    label: &'static str,
    class: &'static str,
) -> impl Widget {
    let state = ui.state;
    let label = i18n::bind_in(desktop, label);
    sapid::button()
        .class(class)
        .label(state.map(move |s| {
            let label = label.get();
            let view = files_view::current(s, remote);
            if view.sort == sort {
                format!("{label} {}", if view.descending { "↓" } else { "↑" })
            } else {
                label
            }
        }))
        .on_press(move || {
            let view = ui.state.with(|s| files_view::current(s, remote));
            files_view::change(&ui, remote, |v| {
                v.sort = Some(sort);
                v.descending = Some(view.sort == sort && !view.descending);
            });
        })
}
fn transfer_button(ui: Ui, remote: bool) -> impl Widget {
    let state = ui.state;
    let label = if remote { "下载" } else { "上传" };
    sapid::button()
        .class("button with-icon primary")
        .name(i18n::bind(label))
        .fill((
            icons::glyph(if remote { Icon::Download } else { Icon::Upload }),
            text().content(i18n::bind(label)),
        ))
        .enabled(state.map(move |s| {
            file_browser::transfer_ready(s) && !file_browser::selected_entries(s, remote).is_empty()
        }))
        .on_press(move || {
            ui.transfer(if remote {
                TransferDirection::Download
            } else {
                TransferDirection::Upload
            })
        })
}

fn pane_actions(ui: Ui, remote: bool, files: bool) -> impl Widget {
    let state = ui.state;
    let filter = ui.clone();
    let menu = ui.clone();
    let filter = when(!files, move || {
        let filter = filter.clone();
        icon_action(
            if remote {
                "筛选远程文件"
            } else {
                "筛选本地文件"
            },
            Icon::Filter,
        )
        .class(state.map(move |s| {
            if if remote {
                s.remote_filter_open
            } else {
                s.local_filter_open
            } {
                "icon-button active"
            } else {
                "icon-button"
            }
            .to_string()
        }))
        .on_press(move || {
            filter.state.revise(|s| {
                if remote {
                    s.remote_filter_open = !s.remote_filter_open
                } else {
                    s.local_filter_open = !s.local_filter_open
                }
            })
        })
    });
    fragment((
        filter,
        col().class("pane-menu-anchor").fill((
            (if files {
                i18n::desktop::icon_action("File view options", Icon::More)
            } else {
                icon_action(
                    if remote {
                        "远程文件操作"
                    } else {
                        "本地文件操作"
                    },
                    Icon::More,
                )
            })
            .class("icon-button")
            .on_press(move || {
                menu.state.revise(|s| {
                    s.host_menu = None;
                    s.group_menu = None;
                    s.file_context = None;
                    s.file_menu = if s.file_menu == Some(remote) {
                        None
                    } else {
                        Some(remote)
                    }
                })
            }),
            when(!files, move || {
                file_context::menu(ui.clone(), remote, false)
            }),
        )),
    ))
}

fn pane(ui: Ui, remote: bool, compact: bool) -> impl Widget {
    let state = ui.state;
    let all = ui.clone();
    let clear = ui.clone();
    col().class(state.map(move |s| {
        let mut class = if file_browser::icons(s, remote) { "file-pane icons-view" } else { "file-pane details-view" }.to_owned();
        if compact {
            class.push_str(" files-pane");
            if files_transfer::upload_ready(s) {
                class.push_str(" files-upload-ready");
            }
        }
        if file_browser::transfer_ready(s) {
            class.push_str(" file-transfer-ready");
        }
        class
    })).fill((
        text().class("file-drop-hint").content(i18n::bind_in(compact, if remote {
            "松开以上传到当前远程目录"
        } else {
            "松开以下载到当前本地目录"
        })),
        when(!compact, {
          let ui = ui.clone();
          move || row().class("pane-heading").fill((
            col().class("pane-heading-copy").fill((
                text().class("pane-title").content(i18n::bind_in(compact, if remote {
                    "远程文件"
                } else {
                    "本地文件"
                })),
                text().class("pane-summary").content(i18n::bind_in(compact, state.map(move |s| {
                    if remote && !s.connected {
                        return if s.connecting {
                            "SFTP · 等待 SSH 认证"
                        } else {
                            "SFTP · 等待 SSH 连接"
                        }
                        .into();
                    }
                    if remote && !s.sftp_available {
                        return "SFTP · 不可用".into();
                    }
                    let count = if remote {
                        s.remote.len()
                    } else {
                        s.local.len()
                    };
                    if remote {
                        format!("SFTP · {count} 个项目")
                    } else {
                        format!("{count} 个项目")
                    }
                }))),
            )),
            transfer_button(ui.clone(), remote),
            file_browser::view_controls(ui.clone(), remote),
          ))
        }),
        when(!compact, {
            let ui = ui.clone();
            move || file_browser::pathbar(ui.clone(), remote, false, ())
        }),
        when(compact, {
            let ui = ui.clone();
            move || fragment((
                text().class("files-icon-pixels hidden").content(state.map(move |s| files_view::current(s, remote).icon_size.to_string())),
                file_context::menu(ui.clone(), remote, true),
            ))
        }),
        when(!compact, {
          let ui = ui.clone();
          move || {
            let all = all.clone();
            let clear = clear.clone();
            row().class("pane-tools").fill((
            text()
                .class("pane-summary")
                .content(i18n::bind_in(compact, state.map(move |s| format!("已选 {} 项", selected(s, remote).len())))),
            btn("全选", move || {
                all.state.revise(|s| {
                    let paths = pane_entries(s, remote)
                        .into_iter()
                        .map(|f| f.path)
                        .collect();
                    *selection(s, remote) = paths;
                })
            }),
            btn("清空", move || {
                clear.state.revise(|s| selection(s, remote).clear())
            }),
            pane_actions(ui.clone(), remote, false),
            ))
          }
        }),
        when(!compact, move || row()
            .class(state.map(move |s| {
                if if remote {
                    s.remote_filter_open
                } else {
                    s.local_filter_open
                } {
                    "filter-row"
                } else {
                    "hidden"
                }
                .into()
            }))
            .fill((
                input()
                    .class("filter-input")
                    .name(i18n::bind_in(compact, if remote {
                        "筛选远程文件"
                    } else {
                        "筛选本地文件"
                    }))
                    .value(state.map(move |s| {
                        if remote {
                            s.remote_filter.clone()
                        } else {
                            s.local_filter.clone()
                        }
                    }))
                    .on(value_changed, move |e| {
                        if let Event::ValueChanged { value, .. } = e {
                            state.revise(|s| {
                                selection(s, remote).clear();
                                if remote {
                                    s.remote_filter = value
                                } else {
                                    s.local_filter = value
                                }
                            });
                        }
                    }),
                text().class("subtle").content(i18n::bind_in(compact, "按文件名筛选")),
            ))),
        row().class("file-header").fill((
            sort_button(ui.clone(), remote, Sort::Name, compact, "名称", "column-name"),
            when(state.map(move |s| file_columns::visible(s, remote).size), { let ui = ui.clone(); move || sort_button(ui.clone(), remote, Sort::Size, compact, "大小", "column-size") }),
            when(state.map(move |s| file_columns::visible(s, remote).kind), move || text().class("column-type").content(i18n::bind_in(compact, "类型"))),
            when(state.map(move |s| file_columns::visible(s, remote).modified), { let ui = ui.clone(); move || sort_button(
                ui.clone(),
                remote,
                Sort::Modified,
                compact,
                "修改时间",
                "column-modified",
            ) }),
            when(state.map(move |s| file_columns::visible(s, remote).permissions), move || text().class("column-permissions").content(i18n::bind_in(compact, "权限"))),
        )),
        file_browser::listing(ui.clone(), remote, compact),
        when(state.map(move |s| pane_entries(s, remote).is_empty()), move || {
            when(state.map(move |s| compact && ready(s, remote)), move || {
                files_ui::empty_listing(state.map(|s| files_search::current(s).has_filter()))
            }).otherwise(move || text().class("file-empty")
            .content(i18n::bind_in(compact, state.map(move |s| {
                if remote && !s.connected {
                    "在右上角连接 SSH 后，即可浏览 SFTP 文件".into()
                } else if remote && !s.sftp_available {
                    "SSH 已连接，但 SFTP 不可用（服务器未启用或当前账号无访问权限）。系统软件、资源概览和系统日志仍可使用。".into()
                } else if !ready(s, remote) {
                    "输入路径后按 Enter".into()
                } else if if remote {
                    !s.remote_filter.is_empty()
                } else {
                    !s.local_filter.is_empty()
                } {
                    "没有匹配的项目".into()
                } else {
                    "此文件夹为空".into()
                }
            }))))
        }),
        file_browser::information(ui.clone(), remote),
    ))
}

fn queue_toggle(ui: Ui, paused: bool) -> sapid::ActionIcon {
    icon_action(
        if paused {
            "继续队列"
        } else {
            "暂停队列"
        },
        if paused { Icon::Play } else { Icon::Pause },
    )
    .on_press(move || {
        ui.state.revise(|s| s.queue_paused = !s.queue_paused);
        ui.schedule();
    })
}
fn dock_toggle(ui: Ui, open: bool) -> sapid::ActionIcon {
    icon_action(
        if open {
            "收起活动面板"
        } else {
            "展开活动面板"
        },
        if open { Icon::Collapse } else { Icon::Expand },
    )
    .on_press(move || ui.state.revise(|s| s.activity_open = !s.activity_open))
}
pub(crate) fn transfer_paused(s: &State, id: &str) -> bool {
    s.transfer_pauses
        .get(id)
        .is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn active_job(job: &TransferJob) -> bool {
    matches!(job.status, TransferStatus::Queued | TransferStatus::Running)
}

fn pause_button(ui: Ui, job: Cage<TransferJob>, paused: bool) -> impl Widget {
    icon_action(
        if paused {
            "继续传输"
        } else {
            "暂停传输"
        },
        if paused { Icon::Play } else { Icon::Pause },
    )
    .class("icon-button job-pause")
    .enabled(ui.state.map(move |s| {
        let job = job.get();
        active_job(&job)
            && !s
                .transfer_cancel(&job.id)
                .is_some_and(|c| c.load(Ordering::Relaxed))
    }))
    .on_press(move || {
        ui.state.revise(|s| {
            let id = job.get().id;
            if !s.jobs.iter().any(|job| job.id == id && active_job(job)) {
                return;
            }
            s.transfer_pauses
                .entry(id.clone())
                .or_default()
                .store(!paused, Ordering::Release);
            s.transfer_now = std::time::Instant::now();
            let waiting = transfer_details::waiting(s, &id);
            if let Some(meter) = s.transfer_meters.get_mut(&id) {
                if paused && !waiting {
                    meter.resume(s.transfer_now);
                } else {
                    meter.pause(s.transfer_now);
                }
            }
        });
        ui.schedule();
    })
}

fn queue_job_visible(s: &State, job: &TransferJob, direction: Option<TransferDirection>) -> bool {
    match direction {
        Some(direction) => {
            job.direction == direction && Some(&job.server_id) == s.selected.as_ref()
        }
        None => browser::queue_visible(&job.status, s.queue_filter),
    }
}

fn queue(ui: Ui, direction: Option<TransferDirection>) -> impl Widget {
    let state = ui.state;
    let pause = ui.clone();
    let retry = ui.clone();
    let clear = ui.clone();
    col().class("queue-content").fill((
        when(direction.is_none(), {
            let ui = ui.clone();
            move || {
                let pause = pause.clone();
                let retry = retry.clone();
                let clear = clear.clone();
                row().class("queue-toolbar").fill((
                    row().class("queue-filters").fill((
                        queue_filter(ui.clone(), 0, "全部"),
                        queue_filter(ui.clone(), 1, "进行中"),
                        queue_filter(ui.clone(), 2, "失败 / 取消"),
                        queue_filter(ui.clone(), 3, "已完成"),
                    )),
                    when(state.map(|s| s.queue_paused), {
                        let pause = pause.clone();
                        move || queue_toggle(pause.clone(), true)
                    })
                    .otherwise(move || queue_toggle(pause.clone(), false)),
                    icon_action("重试失败项", Icon::Refresh)
                        .enabled(state.map(|s| {
                            s.connected
                                && s.jobs.iter().any(|j| {
                                    matches!(
                                        j.status,
                                        TransferStatus::Failed(_) | TransferStatus::Cancelled
                                    )
                                })
                        }))
                        .on_press(move || retry.retry(None)),
                    icon_action("清除已结束的传输", Icon::Clear).on_press(move || {
                        clear.state.revise(|s| {
                            s.jobs.retain(|j| {
                                matches!(j.status, TransferStatus::Running | TransferStatus::Queued)
                            });
                            s.files_uploads.retain(&s.jobs);
                            s.transfer_meters
                                .retain(|id, _| s.jobs.iter().any(|j| &j.id == id));
                            s.transfer_details
                                .retain(|id, _| s.jobs.iter().any(|j| &j.id == id));
                        })
                    }),
                ))
            }
        }),
        col().class("queue-list").fill(
            each(state.map(move |s| {
                s.jobs
                    .iter()
                    .filter(|j| queue_job_visible(s, j, direction))
                    .cloned()
                    .collect::<Vec<_>>()
            }))
            .key(|j| j.id.clone())
            .children({
                let ui = ui.clone();
                move |item| {
                    let job = item.value;
                    let cancel = ui.clone();
                    let retry = ui.clone();
                    col().class("queue-job").fill((
                        row().class("queue-row").fill((
                            transfer_details::toggle(ui.clone(), job),
                            text().class("job-name").content(job.map(|j| {
                                format!(
                                    "{} {}",
                                    if j.direction == TransferDirection::Upload {
                                        "↑"
                                    } else {
                                        "↓"
                                    },
                                    j.local_path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                )
                            })),
                            col().class("job-transfer").fill((
                                row().class("job-progress-heading").fill(
                                    text().class("job-status").content(i18n::bind(state.map(
                                        move |s| {
                                            let j = job.get();
                                            match &j.status {
                                                _ if active_job(&j)
                                                    && transfer_paused(s, &j.id) =>
                                                {
                                                    "已暂停".into()
                                                }
                                                TransferStatus::Queued => "等待中".into(),
                                                TransferStatus::Running
                                                    if transfer_details::waiting(s, &j.id) =>
                                                {
                                                    "等待文件冲突处理".into()
                                                }
                                                TransferStatus::Running if j.total_bytes == 0 => {
                                                    "正在准备传输…".into()
                                                }
                                                TransferStatus::Running
                                                    if j.transferred_bytes >= j.total_bytes =>
                                                {
                                                    "正在完成传输…".into()
                                                }
                                                TransferStatus::Running => format!(
                                                    "{} / {}",
                                                    transfer_progress::bytes(j.transferred_bytes),
                                                    transfer_progress::bytes(j.total_bytes)
                                                ),
                                                TransferStatus::Completed => {
                                                    match s.transfer_summaries.get(&j.id) {
                                                        Some(report)
                                                            if report.transferred_files == 0
                                                                && report.skipped_files > 0 =>
                                                        {
                                                            format!(
                                                                "已跳过 {} 个文件",
                                                                report.skipped_files
                                                            )
                                                        }
                                                        Some(report)
                                                            if report.skipped_files > 0 =>
                                                        {
                                                            format!(
                                                                "已完成 · 跳过 {} 个文件",
                                                                report.skipped_files
                                                            )
                                                        }
                                                        _ => "已完成".into(),
                                                    }
                                                }
                                                TransferStatus::Cancelled => "已取消".into(),
                                                TransferStatus::Failed(e) => format!("失败：{e}"),
                                            }
                                        },
                                    ))),
                                ),
                                text().class("job-transfer-details").content(
                                    state.map(move |s| transfer_progress::details(s, &job.get())),
                                ),
                            )),
                            row().class("job-progress-column").fill((
                                col()
                                    .class(job.map(|j| {
                                        match j.status {
                                            TransferStatus::Failed(_) => "job-progress failed",
                                            TransferStatus::Cancelled => "job-progress cancelled",
                                            _ => "job-progress",
                                        }
                                        .to_owned()
                                    }))
                                    .name(i18n::bind("传输进度"))
                                    .role(glory::render::Role::ProgressBar)
                                    .semantics(job.map(|j| {
                                        Some(Box::new(glory::render::Semantics {
                                            progress: Some(glory::render::ProgressValue {
                                                min: 0.,
                                                max: 100.,
                                                current: Some(transfer_progress::percent(j)),
                                                value_text: None,
                                            }),
                                            ..Default::default()
                                        }))
                                    }))
                                    .fill(
                                        col()
                                            .style(job.map(|j| {
                                                glory::style::Style::new().width(
                                                    glory::style::Length::Percent(
                                                        transfer_progress::percent(j),
                                                    ),
                                                )
                                            }))
                                            .class("job-progress-fill"),
                                    ),
                                text().class("job-percent").content(
                                    job.map(|j| format!("{:.1}%", transfer_progress::percent(j))),
                                ),
                            )),
                            when(state.map(move |s| transfer_paused(s, &job.get().id)), {
                                let ui = ui.clone();
                                move || pause_button(ui.clone(), job, true)
                            })
                            .otherwise({
                                let ui = ui.clone();
                                move || pause_button(ui.clone(), job, false)
                            }),
                            close_action("取消传输")
                                .class("icon-button job-cancel")
                                .enabled(job.map(active_job))
                                .on_press(move || {
                                    cancel.state.revise(|s| {
                                        let id = job.get().id;
                                        if let Some(token) = s.transfer_cancel(&id) {
                                            token.store(true, Ordering::Relaxed);
                                        }
                                        if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id)
                                            && j.status == TransferStatus::Queued
                                        {
                                            j.status = TransferStatus::Cancelled;
                                            s.transfer_pauses.remove(&id);
                                        }
                                    });
                                    cancel.schedule();
                                }),
                            icon_action("重试传输", Icon::Refresh)
                                .class("icon-button job-retry")
                                .enabled(state.map(move |s| {
                                    let j = job.get();
                                    s.connected
                                        && Some(&j.server_id) == s.selected.as_ref()
                                        && !s.cancels.contains_key(&j.id)
                                        && matches!(
                                            j.status,
                                            TransferStatus::Failed(_) | TransferStatus::Cancelled
                                        )
                                }))
                                .on_press(move || retry.retry(Some(job.get().id))),
                        )),
                        transfer_details::listing(ui.clone(), job),
                    ))
                }
            }),
        ),
        text()
            .class(state.map(move |s| {
                if s.jobs.iter().any(|j| queue_job_visible(s, j, direction)) {
                    "hidden"
                } else {
                    "queue-empty"
                }
                .into()
            }))
            .content(i18n::bind("暂无传输任务")),
    ))
}
fn direction_label(direction: TransferDirection) -> &'static str {
    match direction {
        TransferDirection::Upload => "上传详情",
        TransferDirection::Download => "下载详情",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TransferPhase {
    #[default]
    Empty,
    Preparing,
    Running,
    Paused,
    Conflict,
    Completed,
    Failed,
    Cancelled,
}

impl TransferPhase {
    fn class(self) -> &'static str {
        match self {
            Self::Empty => "hidden",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Conflict => "conflict",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Empty | Self::Running => "",
            Self::Preparing => "正在准备传输…",
            Self::Paused => "已暂停",
            Self::Conflict => "等待文件冲突处理",
            Self::Completed => "已完成",
            Self::Failed => "执行失败",
            Self::Cancelled => "已取消",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct TransferIndicator {
    phase: TransferPhase,
    active: usize,
    percent: Option<u8>,
}

fn direction_progress(s: &State, direction: TransferDirection) -> TransferIndicator {
    let jobs = s
        .jobs
        .iter()
        .filter(|job| queue_job_visible(s, job, Some(direction)));
    let Some(latest) = jobs.clone().next_back() else {
        return TransferIndicator::default();
    };
    let active_jobs = jobs.clone().filter(|job| active_job(job));
    let active = active_jobs.clone().count();
    if active == 0 {
        let latest_batch = s.batch_ids.get(&latest.id);
        let mut phase = TransferPhase::Completed;
        for job in jobs.filter(|job| {
            job.id == latest.id
                || (latest_batch.is_some() && s.batch_ids.get(&job.id) == latest_batch)
        }) {
            match job.status {
                TransferStatus::Failed(_) => {
                    phase = TransferPhase::Failed;
                    break;
                }
                TransferStatus::Cancelled => phase = TransferPhase::Cancelled,
                _ => {}
            }
        }
        return TransferIndicator {
            phase,
            ..Default::default()
        };
    }
    let batches: HashSet<_> = active_jobs
        .filter_map(|job| s.batch_ids.get(&job.id))
        .collect();
    let mut total = 0u128;
    let mut done = 0u128;
    let mut unknown = false;
    let mut progressing = false;
    let mut queued = false;
    let mut conflict = false;
    for job in jobs {
        if active_job(job) {
            unknown |= job.total_bytes == 0;
            if !transfer_paused(s, &job.id) {
                match job.status {
                    TransferStatus::Running if transfer_details::waiting(s, &job.id) => {
                        conflict = true
                    }
                    TransferStatus::Running => progressing = true,
                    TransferStatus::Queued if !s.queue_paused => queued = true,
                    _ => {}
                }
            }
            done += u128::from(job.transferred_bytes.min(job.total_bytes));
        } else if job.status == TransferStatus::Completed
            && s.batch_ids
                .get(&job.id)
                .is_some_and(|batch| batches.contains(batch))
        {
            done += u128::from(job.total_bytes);
        } else {
            continue;
        }
        total += u128::from(job.total_bytes);
    }
    let percent = (!unknown && total > 0).then(|| (done * 100 / total).min(99) as u8);
    let phase = if progressing {
        if percent.is_some() {
            TransferPhase::Running
        } else {
            TransferPhase::Preparing
        }
    } else if conflict {
        TransferPhase::Conflict
    } else if queued {
        TransferPhase::Preparing
    } else {
        TransferPhase::Paused
    };
    TransferIndicator {
        phase,
        active,
        percent,
    }
}

pub(crate) fn transfer_progress_buttons(ui: Ui) -> impl Widget {
    fn button(ui: Ui, direction: TransferDirection) -> impl Widget {
        widget(move |scope| {
            let ui = ui.clone();
            let summary = ui
                .state
                .map(move |s| direction_progress(s, direction))
                .with_partial_eq();
            let icon = match direction {
                TransferDirection::Upload => Icon::Upload,
                TransferDirection::Download => Icon::Download,
            };
            let name = scope.bond(move || {
                let snapshot = summary.get();
                let label = i18n::translate(direction_label(direction));
                let status = i18n::translate(snapshot.phase.label());
                let mut parts = vec![label];
                if !status.is_empty() {
                    parts.push(status);
                }
                if let Some(value) = snapshot.percent {
                    parts.push(format!("{value}%"));
                }
                if snapshot.active > 1 {
                    parts.push(i18n::translate(&format!("{} 个传输任务", snapshot.active)));
                }
                parts.join(" · ")
            });
            view()
                .class(
                    scope.bond(move || {
                        format!("transfer-indicator {}", summary.get().phase.class())
                    }),
                )
                .fill((
                    sapid::ring_progress(
                        scope.bond(move || f64::from(summary.get().percent.unwrap_or(0))),
                    )
                    .name(i18n::bind("传输进度"))
                    .size(24.)
                    .thickness(2.)
                    .indeterminate(scope.bond(move || summary.get().percent.is_none()))
                    .animated(scope.bond(move || summary.get().phase == TransferPhase::Preparing)),
                    sapid::button()
                        .class("icon-button transfer-progress-button")
                        .name(name)
                        .fill((
                            icons::glyph(icon),
                            text().class("transfer-count").content(scope.bond(move || {
                                match summary.get().active {
                                    0 | 1 => String::new(),
                                    n if n > 9 => "9+".into(),
                                    n => n.to_string(),
                                }
                            })),
                        ))
                        .on_press(move || ui.open_dialog(Dialog::TransferProgress(direction))),
                ))
                .show_in(scope)
        })
    }

    row().class("desktop-transfers").fill((
        row().class("files-download-drop").fill((
            icons::glyph(Icon::Download),
            text().content(i18n::bind("拖到此处下载")),
        )),
        button(ui.clone(), TransferDirection::Upload),
        button(ui, TransferDirection::Download),
    ))
}

pub(crate) fn transfer_progress_dialog(ui: Ui) -> impl Widget {
    fn details(ui: Ui, direction: TransferDirection) -> impl Widget {
        let state = ui.state;
        when(
            state.map(move |s| s.dialog == Some(Dialog::TransferProgress(direction))),
            move || {
                let close = ui.clone();
                let conflict = ui.clone();
                modal(
                    ui.clone(),
                    direction_label(direction),
                    queue(ui.clone(), Some(direction)),
                    (
                        when(
                            state.map(move |s| {
                                s.transfer_conflict
                                    .as_ref()
                                    .is_some_and(|pending| pending.info.direction == direction)
                            }),
                            move || {
                                let conflict = conflict.clone();
                                sapid::button()
                                    .class("button primary")
                                    .label(i18n::bind("处理文件冲突"))
                                    .on_press(move || conflict.close_dialog())
                            },
                        ),
                        sapid::button()
                            .class("button")
                            .label(i18n::bind("关闭"))
                            .on_press(move || close.close_dialog()),
                    ),
                )
            },
        )
    }
    fragment((
        details(ui.clone(), TransferDirection::Upload),
        details(ui, TransferDirection::Download),
    ))
}

fn queue_filter(ui: Ui, index: u8, label: &'static str) -> impl Widget {
    let state = ui.state;
    sapid::button()
        .class(state.map(move |s| {
            if s.queue_filter == index {
                "quiet-button active"
            } else {
                "quiet-button"
            }
            .to_string()
        }))
        .label(i18n::bind(label))
        .on_press(move || ui.state.revise(|s| s.queue_filter = index))
}
fn tab(ui: Ui, index: u8, label: &'static str) -> impl Widget {
    let state = ui.state;
    sapid::button()
        .class(state.map(move |s| {
            if s.activity_tab == index && s.activity_open {
                "dock-tab active"
            } else {
                "dock-tab"
            }
            .into()
        }))
        .label(i18n::bind(state.map(move |s| {
            if index == 0 {
                format!("传输 ({})", s.jobs.len())
            } else {
                label.into()
            }
        })))
        .on_press(move || {
            ui.state.revise(|s| {
                s.activity_tab = index;
                s.activity_open = true;
            })
        })
}
pub(crate) fn activity(ui: Ui) -> impl Widget {
    let state = ui.state;
    let toggle = ui.clone();
    let logs = ui.clone();
    let queue_ui = ui.clone();
    col()
        .class(state.map(|s| {
            if s.activity_open {
                "activity-dock"
            } else {
                "activity-dock collapsed"
            }
            .into()
        }))
        .fill((
            row().class("dock-header").fill((
                row()
                    .class("dock-tabs")
                    .fill((tab(ui.clone(), 0, "传输"), tab(ui.clone(), 1, "操作日志"))),
                when(state.map(|s| s.activity_open), {
                    let toggle = toggle.clone();
                    move || dock_toggle(toggle.clone(), true)
                })
                .otherwise(move || dock_toggle(toggle.clone(), false)),
            )),
            col()
                .class(state.map(|s| {
                    if s.activity_open {
                        "dock-content"
                    } else {
                        "hidden"
                    }
                    .into()
                }))
                .fill((
                    when(state.map(|s| s.activity_tab == 0), move || {
                        queue(queue_ui.clone(), None)
                    }),
                    when(state.map(|s| s.activity_tab == 1), move || {
                        text()
                            .class("logs")
                            .selectable(true)
                            .content(logs.state.map(|s| s.logs.clone()))
                    }),
                )),
        ))
}

#[cfg(test)]
mod transfer_indicator_tests {
    use super::*;

    #[test]
    fn concurrent_transfers_share_byte_progress_only_when_all_sizes_are_known() {
        let mut s = State::new(AppConfig::default(), String::new(), String::new());
        let direction = TransferDirection::Upload;
        s.selected = Some("server".into());
        let job = TransferJob {
            id: "small".into(),
            server_id: "server".into(),
            local_path: "picture.png".into(),
            remote_path: "/picture.png".into(),
            direction,
            status: TransferStatus::Running,
            total_bytes: 100,
            transferred_bytes: 50,
        };
        let mut large = job.clone();
        large.id = "large".into();
        large.total_bytes = 0;
        large.transferred_bytes = 0;
        enqueue_transfers(&mut s, vec![job, large]);
        let summary = direction_progress(&s, direction);
        assert_eq!(summary.active, 2);
        assert!(summary.phase == TransferPhase::Preparing && summary.percent.is_none());
        assert!(direction_progress(&s, TransferDirection::Download).phase == TransferPhase::Empty);
        s.jobs[1].total_bytes = 900;
        assert_eq!(direction_progress(&s, direction).percent, Some(5));
        s.jobs[0].status = TransferStatus::Completed;
        assert_eq!(direction_progress(&s, direction).percent, Some(10));
        s.jobs[1].status = TransferStatus::Failed("disconnected".into());
        assert!(direction_progress(&s, direction).phase == TransferPhase::Failed);
    }
}
