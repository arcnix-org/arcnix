use crate::*;

pub(crate) fn new_server(ui: &Ui) {
    ui.navigate(navigation::Page::Servers);
    ui.state.revise(|s| {
        s.editor = ServerProfile::default();
        s.server_editor_original = None;
        s.server_tags.clear();
        s.editor.group_id = s.group.clone().filter(|id| !id.is_empty());
        s.port = "22".into();
    });
    ui.open_dialog(Dialog::Server);
}

fn new_group(ui: &Ui) {
    ui.state.revise(|s| s.group_name.clear());
    ui.open_dialog(Dialog::Group { id: None });
}

pub(crate) fn edit_server(ui: &Ui, id: &str) {
    let profile = ui
        .state
        .with(|s| s.config.available_servers().find(|p| p.id == id).cloned());
    if let Some(profile) = profile {
        ui.state.revise(|s| {
            s.port = profile.port.to_string();
            s.server_editor_original = Some(profile.clone());
            s.server_tags = profile.tags.join(", ");
            s.editor = profile;
        });
        ui.open_dialog(Dialog::Server);
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeDrop {
    action: TreeAction,
    #[serde(rename = "sequence")]
    _sequence: u64,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TreeAction {
    Reorder {
        group: String,
        target: String,
        after: bool,
    },
    MoveServer {
        server: String,
        target: Option<String>,
    },
}

fn apply_tree_drop(ui: &Ui, value: &str) {
    if value.len() > 2048 {
        return;
    }
    let Ok(gesture) = serde_json::from_str::<TreeDrop>(value) else {
        return;
    };
    if ui.state.with(|s| tree_filtered(s) || s.dialog.is_some()) {
        return;
    }
    let result = ui.state.with(|s| -> anyhow::Result<Option<AppConfig>> {
        let mut config = s.config.clone();
        let changed = match &gesture.action {
            TreeAction::Reorder {
                group,
                target,
                after,
            } => config.move_group(group, target, *after)?,
            TreeAction::MoveServer { server, target } => {
                if target
                    .as_ref()
                    .is_some_and(|id| !config.groups.iter().any(|g| &g.id == id))
                {
                    return Ok(None);
                }
                let Some(profile) = config
                    .servers
                    .iter_mut()
                    .find(|p| &p.id == server && p.available_on_this_platform())
                else {
                    return Ok(None);
                };
                if &profile.group_id == target {
                    return Ok(None);
                }
                profile.group_id = target.clone();
                true
            }
        };
        if !changed {
            return Ok(None);
        }
        persist(ui, &config)?;
        Ok(Some(config))
    });
    match result {
        Ok(Some(config)) => ui.state.revise(|s| {
            s.config = config;
            if let TreeAction::MoveServer { server, target } = gesture.action {
                if let Some(id) = &target {
                    s.collapsed_groups.remove(id);
                }
                if s.selected.as_ref() == Some(&server) {
                    s.group = target;
                }
            }
        }),
        Ok(None) => {}
        Err(error) => match gesture.action {
            TreeAction::Reorder { .. } => ui.error(format!("无法保存分组顺序：{error:#}")),
            TreeAction::MoveServer { .. } => ui.error(format!("无法移动服务器：{error:#}")),
        },
    }
}

fn tree_filtered(s: &State) -> bool {
    s.connected_servers_only || !s.search.trim().is_empty()
}

fn expanded(s: &State, id: &str) -> bool {
    tree_filtered(s) || !s.collapsed_groups.contains(id)
}

fn matching_servers<'a>(
    s: &'a State,
    group: Option<&'a str>,
) -> impl Iterator<Item = &'a ServerProfile> {
    let query = s.search.trim();
    let group_name = group
        .and_then(|id| s.config.groups.iter().find(|g| g.id == id))
        .map(|g| g.name.as_str())
        .unwrap_or_default();
    s.config.available_servers().filter(move |p| {
        p.group_id.as_deref() == group
            && (!s.connected_servers_only || s.server_connection(&p.id).0)
            && (query.is_empty() || arcnix_core::server_labels::matches(p, group_name, query))
    })
}

fn group_menu(ui: Ui, id: String) -> impl Widget {
    let state = ui.state;
    let add = ui.clone();
    let rename = ui.clone();
    let remove = ui.clone();
    let destroy = ui.clone();
    let toggle = ui.clone();
    let add_id = id.clone();
    let rename_id = id.clone();
    let delete_id = id.clone();
    let destroy_id = id.clone();
    let toggle_id = id.clone();
    let guarded_id = id.clone();
    col().class("context-popover group-context-menu").fill((
        btn("新建服务器", move || {
            add.state.revise(|s| s.group = Some(add_id.clone()));
            new_server(&add);
        })
        .class("menu-item"),
        sapid::button()
            .class("menu-item")
            .label(i18n::bind(state.map(move |s| {
                if expanded(s, &id) {
                    "折叠分组"
                } else {
                    "展开分组"
                }
                .to_owned()
            })))
            .enabled(state.map(|s| !tree_filtered(s)))
            .on_press(move || {
                toggle.state.revise(|s| {
                    if !s.collapsed_groups.remove(&toggle_id) {
                        s.collapsed_groups.insert(toggle_id.clone());
                    }
                    s.group_menu = None;
                })
            }),
        btn("重命名分组", move || {
            if let Some(name) = state.with(|s| {
                s.config
                    .groups
                    .iter()
                    .find(|g| g.id == rename_id)
                    .map(|g| g.name.clone())
            }) {
                rename.state.revise(|s| s.group_name = name);
                rename.open_dialog(Dialog::Group {
                    id: Some(rename_id.clone()),
                });
            }
        })
        .class("menu-item"),
        btn("解散分组（保留服务器）", move || {
            remove.open_dialog(Dialog::DissolveGroup(delete_id.clone()))
        })
        .class("menu-item"),
        btn("删除分组及服务器", move || {
            destroy.open_dialog(Dialog::DeleteGroupAndServers(destroy_id.clone()))
        })
        .class("menu-item danger")
        .enabled(state.map({
            let id = guarded_id.clone();
            move |s| {
                !s.system_task_running
                    || !s.config.available_servers().any(|p| {
                        s.selected.as_deref() == Some(&p.id) && p.group_id.as_deref() == Some(&id)
                    })
            }
        })),
    ))
}

fn root_menu(ui: Ui) -> impl Widget {
    let add = ui.clone();
    let group = ui.clone();
    let import = ui.clone();
    let expand = ui.clone();
    let collapse = ui.clone();
    col().class("context-popover tree-root-menu").fill((
        btn("新建服务器", move || {
            add.state.revise(|s| s.group = None);
            new_server(&add);
        })
        .class("menu-item"),
        btn("新建分组", move || new_group(&group)).class("menu-item"),
        btn("导入 SSH 配置", move || {
            import.open_dialog(Dialog::Import)
        })
        .class("menu-item"),
        btn("展开全部", move || {
            expand.state.revise(|s| {
                s.collapsed_groups.clear();
                s.group_menu = None;
            })
        })
        .class("menu-item"),
        btn("折叠全部", move || {
            collapse.state.revise(|s| {
                s.collapsed_groups = s.config.groups.iter().map(|g| g.id.clone()).collect();
                s.search.clear();
                s.connected_servers_only = false;
                s.group_menu = None;
            })
        })
        .class("menu-item"),
    ))
}

pub(crate) fn sidebar(ui: Ui) -> impl Widget {
    let state = ui.state;
    let add = ui.clone();
    let group = ui.clone();
    let menu = ui.clone();
    let groups = state
        .map(|s| {
            let query = s.search.trim().to_lowercase();
            s.config
                .groups
                .iter()
                .filter(|g| {
                    (!s.connected_servers_only && g.name.to_lowercase().contains(&query))
                        || matching_servers(s, Some(&g.id)).next().is_some()
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .with_partial_eq();
    let group_counts = state
        .map(|s| {
            let mut counts = HashMap::<String, (usize, usize)>::new();
            for server in s.config.available_servers() {
                if let Some(group) = &server.group_id {
                    let (total, connected) = counts.entry(group.clone()).or_default();
                    *total += 1;
                    *connected += usize::from(s.server_connection(&server.id).0);
                }
            }
            counts
        })
        .with_partial_eq();
    col().class("sidebar server-explorer").fill((
        row()
            .class(state.map(|s| {
                if s.group_menu.as_deref() == Some("") {
                    "sidebar-heading tree-root-row menu-open"
                } else {
                    "sidebar-heading tree-root-row"
                }
                .to_owned()
            }))
            .fill((
                text().class("sidebar-title").content(i18n::bind("服务器")),
                row().class("tree-actions").fill((
                    icon_action("新建服务器", Icon::Plus)
                        .class("icon-button tree-new-server")
                        .on_press(move || new_server(&add)),
                    icon_action("新建分组", Icon::Folder)
                        .class("icon-button tree-new-group")
                        .on_press(move || new_group(&group)),
                    icon_action("服务器列表操作", Icon::More)
                        .class("icon-button tree-more")
                        .on_press(move || {
                            menu.state.revise(|s| {
                                s.host_menu = None;
                                s.group_menu = if s.group_menu.as_deref() == Some("") {
                                    None
                                } else {
                                    Some(String::new())
                                };
                            })
                        }),
                )),
                when(state.map(|s| s.group_menu.as_deref() == Some("")), {
                    let ui = ui.clone();
                    move || root_menu(ui.clone())
                }),
            )),
        navigation::local_desktop_entry(ui.clone()),
        row().class("server-search").fill((
            input()
                .name(i18n::bind("搜索"))
                .value(state.map(|s| s.search.clone()))
                .on(value_changed, move |event| {
                    if let Event::ValueChanged { value, .. } = event {
                        state.revise(|s| s.search = value);
                    }
                }),
            text()
                .class("action-glyph glyph-search server-search-icon")
                .content(""),
        )),
        sapid::checkbox()
            .class("preference-check tree-connected-filter")
            .label(i18n::bind("只看已连接"))
            .checked(state.map(|s| s.connected_servers_only))
            .on_checked_change(move |checked| state.revise(|s| s.connected_servers_only = checked)),
        input()
            .class("server-tree-input hidden")
            .value("")
            .on(value_changed, {
                let ui = ui.clone();
                move |event| {
                    if let Event::ValueChanged { value, .. } = event {
                        apply_tree_drop(&ui, &value);
                    }
                }
            }),
        col()
            .class(state.map(|s| {
                if !tree_filtered(s) {
                    "server-tree server-list"
                } else {
                    "server-tree server-list reorder-disabled"
                }
                .to_owned()
            }))
            .name(i18n::bind("服务器树"))
            .fill((
                each(groups)
                .key(|g| (g.id.clone(), g.name.clone()))
                .children({
                    let ui = ui.clone();
                    move |item| {
                        let g = item.value;
                        let choose = ui.clone();
                        let menu = ui.clone();
                        col().class("tree-folder").fill((
                            row()
                                .class(state.map(move |s| {
                                    format!(
                                        "group-row tree-folder-row{}{}{}",
                                        if expanded(s, &g.get().id) {
                                            " expanded"
                                        } else {
                                            ""
                                        },
                                        if s.group.as_deref() == Some(&g.get().id) {
                                            " selected"
                                        } else {
                                            ""
                                        },
                                        if s.group_menu.as_deref() == Some(&g.get().id) {
                                            " menu-open"
                                        } else {
                                            ""
                                        }
                                    )
                                }))
                                .fill((
                                    text()
                                        .class("tree-group-id hidden")
                                        .content(g.map(|g| g.id.clone())),
                                    sapid::button()
                                        .class("group-button")
                                        .name(g.map(|g| g.name.clone()))
                                        .fill((
                                            text()
                                                .class(
                                                    "tree-chevron action-glyph glyph-chevron-right",
                                                )
                                                .content(""),
                                            icons::glyph(Icon::Folder),
                                            text()
                                                .class("tree-folder-name")
                                                .content(g.map(|g| g.name.clone())),
                                            text().class("tree-count").content(g.map(move |group| {
                                                group_counts.with(|counts| counts.get(&group.id).map_or(0, |c| c.0)).to_string()
                                            })),
                                        ))
                                        .on_press(move || {
                                            choose.state.revise(|s| {
                                                let id = g.get().id;
                                                s.group = Some(id.clone());
                                                s.host_menu = None;
                                                s.group_menu = None;
                                                if !tree_filtered(s)
                                                    && !s.collapsed_groups.remove(&id)
                                                {
                                                    s.collapsed_groups.insert(id);
                                                }
                                            })
                                        }),
                                    text().class("tree-connection group-connection").content(
                                        i18n::bind(g.map(move |group| group_counts.with(|counts| match counts.get(&group.id) {
                                            Some(&(_, n)) if n > 0 => format!("{n} 台已连接"),
                                            _ => String::new(),
                                        })))
                                    ),
                                    icon_action(format!("{}的分组操作", g.get().name), Icon::More)
                                        .class("icon-button group-more")
                                        .on_press(move || {
                                            menu.state.revise(|s| {
                                                let id = g.get().id;
                                                s.group = Some(id.clone());
                                                s.host_menu = None;
                                                s.group_menu = if s.group_menu.as_ref() == Some(&id)
                                                {
                                                    None
                                                } else {
                                                    Some(id)
                                                };
                                            })
                                        }),
                                    when(
                                        state.map(move |s| {
                                            s.group_menu.as_deref() == Some(&g.get().id)
                                        }),
                                        {
                                            let ui = ui.clone();
                                            move || group_menu(ui.clone(), g.get().id)
                                        },
                                    ),
                                )),
                            when(state.map(move |s| expanded(s, &g.get().id)), {
                                let ui = ui.clone();
                                move || {
                                    let id = g.get().id;
                                    col().class("tree-children").fill((
                                        server_rows(ui.clone(), Some(id.clone())),
                                        when(
                                            state.map(move |s| {
                                                matching_servers(s, Some(&id)).next().is_none()
                                            }),
                                            || {
                                                text()
                                                    .class("tree-empty-group")
                                                    .content(i18n::bind("空分组"))
                                            },
                                        ),
                                    ))
                                }
                            }),
                        ))
                    }
                }),
                server_rows(ui.clone(), None),
                when(
                    state.map(move |s| tree_filtered(s) && groups.with(Vec::is_empty) && matching_servers(s, None).next().is_none()),
                    move || {
                        text()
                            .class("tree-no-results")
                            .content(i18n::bind(state.map(|s| {
                                if s.connected_servers_only { "没有符合条件的已连接服务器" } else { "未找到匹配的服务器或分组" }.to_owned()
                            })))
                    },
                ),
                when(
                    state.map(|s| {
                        !tree_filtered(s) && s.config.available_servers().next().is_none() && s.config.groups.is_empty()
                    }),
                    || {
                        text()
                            .class("tree-empty-state")
                            .content(i18n::bind("新建分组来整理服务器，或添加第一台服务器。"))
                    },
                ),
            )),
        text()
            .class("tree-footer")
            .content(i18n::bind(state.map(|s| {
                format!(
                    "{} 个分组 · {} 台服务器",
                    s.config.groups.len(),
                    s.config.available_servers().count()
                )
            }))),
    ))
}

fn server_rows(ui: Ui, group: Option<String>) -> impl Widget {
    let state = ui.state;
    let rows = state.map(move |s| {
        matching_servers(s, group.as_deref())
            .cloned()
            .collect::<Vec<_>>()
    });
    each(rows)
        .key(|p| (p.id.clone(), p.name.clone(), p.host.clone(), p.port))
        .children({
            let ui = ui.clone();
            move |item| {
                let value = item.value;
                let select = ui.clone();
                let menu = ui.clone();
                let edit = ui.clone();
                let delete = ui.clone();
                let connect = ui.clone();
                let reset = ui.clone();
                row()
                    .class(state.map(move |s| {
                        format!(
                            "host-row{}{}",
                            if s.selected.as_deref() == Some(&value.get().id) {
                                " selected"
                            } else {
                                ""
                            },
                            if s.host_menu.as_deref() == Some(&value.get().id) {
                                " menu-open"
                            } else {
                                ""
                            }
                        )
                    }))
                    .fill((
                        text()
                            .class("tree-server-id hidden")
                            .content(value.map(|p| p.id.clone())),
                        icons::glyph(Icon::Server),
                        sapid::button()
                            .class("host-select")
                            .enabled(state.map(move |s| {
                                !s.system_task_running
                                    || s.selected.as_deref() == Some(&value.get().id)
                            }))
                            .name(value.map(|p| {
                                #[cfg(windows)]
                                if let Some(distro) = p.direct_wsl() {
                                    return format!("{}，WSL · {distro}", p.name);
                                }
                                format!("{}，{}@{}:{}", p.name, p.username, p.host, p.port)
                            }))
                            .fill((
                                text()
                                    .class("host-name")
                                    .content(value.map(|p| p.name.clone())),
                                text().class("host-address").content(value.map(|p| {
                                    #[cfg(windows)]
                                    if let Some(distro) = p.direct_wsl() {
                                        return format!("WSL · {distro}");
                                    }
                                    format!("{}@{}:{}", p.username, p.host, p.port)
                                })),
                                when(value.map(|p| !p.environment.is_empty()), move || {
                                    text()
                                        .class("server-environment")
                                        .content(value.map(|p| p.environment.clone()))
                                }),
                            ))
                            .on_press(move || {
                                let id = value.get().id;
                                select.select_server(id, false);
                                state.revise(|s| s.group = value.get().group_id);
                            }),
                        text()
                            .class(state.map(move |s| {
                                match s.server_connection(&value.get().id) {
                                    (false, true) => "tree-connection host-connection pending",
                                    _ => "tree-connection host-connection",
                                }
                                .to_owned()
                            }))
                            .content(i18n::bind(state.map(move |s| {
                                match s.server_connection(&value.get().id) {
                                    (true, _) => "已连接",
                                    (_, true) => "连接中",
                                    _ => "",
                                }
                                .to_owned()
                            }))),
                        icon_action(format!("{}的服务器操作", value.get().name), Icon::More)
                            .class("icon-button host-more")
                            .on_press(move || {
                                menu.state.revise(|s| {
                                    let id = value.get().id;
                                    s.group_menu = None;
                                    s.host_menu = if s.host_menu.as_ref() == Some(&id) {
                                        None
                                    } else {
                                        Some(id)
                                    };
                                })
                            }),
                        when(
                            state.map(move |s| s.host_menu.as_deref() == Some(&value.get().id)),
                            move || {
                                let edit = edit.clone();
                                let delete = delete.clone();
                                let connect = connect.clone();
                                let reset = reset.clone();
                                let forget = connect.clone();
                                col().class("context-popover").fill((
                                    sapid::button()
                                        .class("menu-item")
                                        .label(i18n::bind(state.map(move |s| {
                                            match s.server_connection(&value.get().id) {
                                                (true, _) => "断开连接",
                                                (_, true) => "取消连接",
                                                _ => "连接服务器",
                                            }
                                            .to_owned()
                                        })))
                                        .enabled(state.map(|s| !s.system_task_running))
                                        .on_press(move || {
                                            let id = value.get().id;
                                            if connect.state.with(|s| {
                                                let (connected, connecting) =
                                                    s.server_connection(&id);
                                                connected || connecting
                                            }) {
                                                connect.disconnect_server(id);
                                            } else {
                                                connect.select_server(id, true);
                                            }
                                            connect.state.revise(|s| s.host_menu = None);
                                        }),
                                    when(
                                        state.map(move |s| {
                                            ssh_credentials::SUPPORTED
                                                && ssh_credentials::Key::for_profile(
                                                    &s.config,
                                                    &value.get(),
                                                )
                                                .is_some()
                                        }),
                                        move || {
                                            let forget = forget.clone();
                                            btn("Forget saved SSH credential", move || {
                                                let key = state.with(|s| {
                                                    ssh_credentials::Key::for_profile(
                                                        &s.config,
                                                        &value.get(),
                                                    )
                                                });
                                                if let Some(key) = key {
                                                    forget.send(Action::ForgetCredentials(
                                                        vec![key],
                                                        true,
                                                    ));
                                                }
                                                state.revise(|s| s.host_menu = None);
                                            })
                                            .class("menu-item")
                                        },
                                    ),
                                    btn("编辑服务器", move || {
                                        edit_server(&edit, &value.get().id)
                                    })
                                    .class("menu-item"),
                                    btn("重置文件冲突选择", move || {
                                        reset.reset_conflict_rules(&value.get().id)
                                    })
                                    .class("menu-item"),
                                    btn("删除服务器", move || {
                                        delete.open_dialog(Dialog::DeleteServer(value.get().id))
                                    })
                                    .class("menu-item danger"),
                                ))
                            },
                        ),
                    ))
            }
        })
}

fn cancel(ui: Ui) -> sapid::Button {
    let state = ui.state;
    btn("取消", move || ui.close_dialog())
        .class("button quiet")
        .enabled(state.map(|s| s.file_pending.is_none() && s.import_pending.is_none()))
}

fn persist(ui: &Ui, config: &AppConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        ui.writable.get(),
        "当前配置读取失败，不能覆盖。请修复配置后重启。"
    );
    ui.store.save(config)
}

fn server_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    let title = state.with(|s| {
        if s.config.available_servers().any(|p| p.id == s.editor.id) {
            "编辑服务器"
        } else {
            "新建服务器"
        }
    });
    let ungrouped = ui.clone();
    let save = ui.clone();
    let general = col()
        .class(state.map(|s| {
            if s.server_editor_tab == 0 {
                "form-stack"
            } else {
                "hidden"
            }
            .to_owned()
        }))
        .fill((
            field(
                "名称",
                &ui,
                |s| s.editor.name.clone(),
                |s, v| s.editor.name = v,
            ),
            row().class("form-row host-port").fill((
                when(state.map(|s| s.editor.wsl_distribution.is_none()), {
                    let ui = ui.clone();
                    move || {
                        field(
                            "主机地址",
                            &ui,
                            |s| s.editor.host.clone(),
                            |s, v| s.editor.host = v,
                        )
                    }
                }),
                #[cfg(windows)]
                when(
                    state.map(|s| s.editor.wsl_distribution.is_some()),
                    move || {
                        col().class("field").fill((
                            text()
                                .class("field-label")
                                .content(i18n::bind("本机 WSL 发行版")),
                            text()
                                .content(state.map(|s| {
                                    s.editor.wsl_distribution.clone().unwrap_or_default()
                                })),
                        ))
                    },
                ),
                when(state.map(|s| s.editor.direct_wsl().is_none()), {
                    let ui = ui.clone();
                    move || field("端口", &ui, |s| s.port.clone(), |s, v| s.port = v)
                }),
            )),
            #[cfg(windows)]
            when(state.map(|s| s.editor.wsl_distribution.is_some()), {
                let ui = ui.clone();
                move || {
                    settings::select(
                        &ui,
                        false,
                        "WSL connection method",
                        [(false, "本机直连"), (true, "SSH")],
                        |s| s.editor.wsl_use_ssh,
                        |_| true,
                        |ui, ssh| ui.state.revise(|s| s.editor.wsl_use_ssh = ssh),
                    )
                }
            }),
            col()
                .class(state.map(|s| {
                    if s.editor.direct_wsl().is_some() {
                        "hidden"
                    } else {
                        "form-stack"
                    }
                    .to_owned()
                }))
                .fill((
                    field(
                        "用户名",
                        &ui,
                        |s| s.editor.username.clone(),
                        |s, v| s.editor.username = v,
                    ),
                    col().class("field").fill((
                        text().class("field-label").content(i18n::bind("认证方式")),
                        settings::select(
                            &ui,
                            false,
                            "Authentication",
                            [
                                (AuthMethod::Password, "密码"),
                                (AuthMethod::PrivateKey, "私钥"),
                            ],
                            |s| s.editor.auth.clone(),
                            |_| true,
                            |ui, auth| ui.state.revise(|s| s.editor.auth = auth),
                        ),
                    )),
                    when(state.map(|s| s.editor.auth == AuthMethod::PrivateKey), {
                        let ui = ui.clone();
                        move || file_picker::field(ui.clone())
                    }),
                )),
            col().class("field").fill((
                text().class("field-label").content(i18n::bind("分组")),
                row().class("choice-list").fill((
                    sapid::button()
                        .class(state.map(|s| {
                            if s.editor.group_id.is_none() {
                                "choice active"
                            } else {
                                "choice"
                            }
                            .to_owned()
                        }))
                        .label(i18n::bind("未分组"))
                        .on_press(move || ungrouped.state.revise(|s| s.editor.group_id = None)),
                    each(state.map(|s| s.config.groups.clone()))
                        .key(|g| g.id.clone())
                        .children({
                            let ui = ui.clone();
                            move |item| {
                                let value = item.value;
                                let ui = ui.clone();
                                sapid::button()
                                    .class(state.map(move |s| {
                                        if s.editor.group_id.as_deref() == Some(&value.get().id) {
                                            "choice active"
                                        } else {
                                            "choice"
                                        }
                                        .to_owned()
                                    }))
                                    .label(value.map(|g| g.name.clone()))
                                    .on_press(move || {
                                        ui.state
                                            .revise(|s| s.editor.group_id = Some(value.get().id))
                                    })
                            }
                        }),
                )),
            )),
            text()
                .class("subtle")
                .content(i18n::bind(state.map(|_state| {
                    #[cfg(windows)]
                    if _state.editor.direct_wsl().is_some() {
                        return "本机直连使用发行版的默认 Linux 账号。".to_owned();
                    }
                    "密码与私钥口令将在连接时输入。".to_owned()
                }))),
        ));
    let body = fragment((
        preferences_ui::server_tabs(ui.clone()),
        general,
        preferences_ui::server_files(ui.clone()),
        preferences_ui::server_connection(ui.clone()),
        server_labels_ui::editor(ui.clone()),
    ));
    modal(
        ui.clone(),
        title,
        body,
        (
            cancel(ui),
            btn("保存服务器", move || {
                let result =
                    save.state
                        .with(|s| -> anyhow::Result<(AppConfig, ServerProfile, bool)> {
                            let mut profile = s.editor.clone();
                            profile.name = profile.name.trim().into();
                            profile.host = profile.host.trim().into();
                            profile.username = profile.username.trim().into();
                            profile.port = s
                                .port
                                .trim()
                                .parse()
                                .map_err(|_| anyhow::anyhow!("端口应为 1–65535 的数字"))?;
                            profile.environment = profile.environment.trim().into();
                            profile.tags = arcnix_core::server_labels::parse_tags(&s.server_tags)?;
                            let labels_only =
                                s.server_editor_original.as_ref().is_some_and(|original| {
                                    arcnix_core::server_labels::same_except_labels(
                                        original, &profile,
                                    )
                                });
                            anyhow::ensure!(
                                labels_only
                                    || !s.system_task_running
                                    || s.selected.as_ref() != Some(&profile.id),
                                "系统软件任务正在执行，完成后才能修改当前服务器"
                            );
                            if labels_only {
                                let mut current = s
                                    .config
                                    .available_servers()
                                    .find(|p| p.id == profile.id)
                                    .cloned()
                                    .ok_or_else(|| {
                                        anyhow::anyhow!("Original server no longer exists")
                                    })?;
                                current.environment = profile.environment;
                                current.tags = profile.tags;
                                profile = current;
                            }
                            let mut config = s.config.clone();
                            config.upsert_server(profile.clone())?;
                            persist(&save, &config)?;
                            Ok((config, profile, labels_only))
                        });
                match result {
                    Ok((config, profile, labels_only)) => {
                        save.forget_changed_credentials(&config);
                        let same = save
                            .state
                            .with(|s| s.selected.as_ref() == Some(&profile.id));
                        let select = same || save.state.with(|s| !s.connected && !s.connecting);
                        if !labels_only {
                            if same {
                                save.disconnect();
                            } else {
                                save.discard_parked(&Some(profile.id.clone()));
                            }
                        }
                        save.state.revise(|s| {
                            s.config = config;
                            // Reveal the saved destination even if its folder was collapsed.
                            s.group = Some(profile.group_id.clone().unwrap_or_default());
                            if let Some(group) = &profile.group_id {
                                s.collapsed_groups.remove(group);
                            }
                            s.search.clear();
                            s.connected_servers_only = false;
                            if !labels_only {
                                s.jobs.retain(|j| {
                                    j.server_id != profile.id
                                        || j.status == TransferStatus::Completed
                                });
                            }
                            if same {
                                s.file_split =
                                    f64::from(profile.file_pane_ratio.unwrap_or(500)) / 1000.0;
                            }
                            s.dialog = None;
                            s.dialog_error.clear();
                            s.notice.clear();
                        });
                        if select && !same {
                            save.select_server(profile.id, false);
                        }
                    }
                    Err(error) => save.state.revise(|s| s.dialog_error = format!("{error:#}")),
                }
            })
            .class("button primary"),
        ),
    )
}

fn group_dialog(ui: Ui) -> impl Widget {
    let id = ui.state.with(|s| match &s.dialog {
        Some(Dialog::Group { id }) => id.clone(),
        _ => None,
    });
    let title = if id.is_some() {
        "重命名分组"
    } else {
        "新建分组"
    };
    let save = ui.clone();
    modal(
        ui.clone(),
        title,
        field(
            "分组名称",
            &ui,
            |s| s.group_name.clone(),
            |s, v| s.group_name = v,
        ),
        (
            cancel(ui),
            btn("保存分组", move || {
                let result = save.state.with(|s| -> anyhow::Result<(AppConfig, String)> {
                    let mut config = s.config.clone();
                    let group_id = if let Some(id) = &id {
                        config.rename_group(id, s.group_name.clone())?;
                        id.clone()
                    } else {
                        config.add_group(s.group_name.clone())?
                    };
                    persist(&save, &config)?;
                    Ok((config, group_id))
                });
                match result {
                    Ok((config, group_id)) => save.state.revise(|s| {
                        s.config = config;
                        s.collapsed_groups.remove(&group_id);
                        s.group = Some(group_id);
                        s.search.clear();
                        s.connected_servers_only = false;
                        s.dialog = None;
                        s.notice.clear();
                    }),
                    Err(error) => save.state.revise(|s| s.dialog_error = format!("{error:#}")),
                }
            })
            .class("button primary"),
        ),
    )
}

fn import_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    let submit = ui.clone();
    modal(
        ui.clone(),
        "导入 SSH 配置",
        col().class("form-stack").fill((
            text().class("subtle").content(i18n::bind(
                "导入 Host 站点与密钥路径，不读取私钥内容或执行配置命令。",
            )),
            field(
                "SSH 配置文件路径",
                &ui,
                |s| s.import_path.clone(),
                |s, v| s.import_path = v,
            ),
            text()
                .class("subtle")
                .content(i18n::bind("不支持的代理、Include 和 Match 会报告并跳过。")),
        )),
        (
            cancel(ui),
            sapid::button()
                .class("button primary")
                .label(i18n::bind(state.map(|s| {
                    if s.import_pending.is_some() {
                        "正在导入…"
                    } else {
                        "导入站点"
                    }
                    .to_owned()
                })))
                .enabled(state.map(|s| s.import_pending.is_none()))
                .on_press(move || {
                    let path = submit.state.with(|s| s.import_path.trim().to_owned());
                    if path.is_empty() {
                        submit
                            .state
                            .revise(|s| s.dialog_error = "请输入 SSH 配置文件路径".into());
                        return;
                    }
                    let serial = submit.send(Action::ImportSshConfig(path.into()));
                    submit.state.revise(|s| {
                        s.import_pending = Some(serial);
                        s.dialog_error.clear();
                    });
                }),
        ),
    )
}

fn connect_dialog(ui: Ui) -> impl Widget {
    let state = ui.state;
    let connect = ui.clone();
    let enter = ui.clone();
    modal(
        ui.clone(),
        "连接服务器",
        col().class("form-stack").fill((
            text()
                .class("dialog-description")
                .content(i18n::bind(state.map(|s| {
                    s.config
                        .available_servers()
                        .find(|p| Some(&p.id) == s.selected.as_ref())
                        .map(|p| format!("{}\n{}@{}:{}", p.name, p.username, p.host, p.port))
                        .unwrap_or_else(|| "请选择服务器".into())
                }))),
            col().class("field").fill((
                text()
                    .class("field-label")
                    .content(i18n::bind(state.map(|s| {
                        if s.config
                            .available_servers()
                            .find(|p| Some(&p.id) == s.selected.as_ref())
                            .is_some_and(|p| p.auth == AuthMethod::PrivateKey)
                        {
                            "私钥口令（未加密可留空）"
                        } else {
                            "密码"
                        }
                        .to_owned()
                    }))),
                input()
                    .name(i18n::bind("密码或私钥口令"))
                    .purpose(Purpose::Password)
                    .value(state.map(|s| s.secret.clone()))
                    .on(value_changed, move |event| {
                        if let Event::ValueChanged { value, .. } = event {
                            state.revise(|s| s.secret = value);
                        }
                    })
                    .on(commit, move |event| {
                        if let Event::Commit { value, .. } = event {
                            enter.state.revise(|s| s.secret = value);
                            begin_connect(&enter);
                        }
                    }),
            )),
            when(state.map(|_| ssh_credentials::SUPPORTED), move || {
                sapid::checkbox()
                    .class("preference-check")
                    .label(i18n::bind("Remember on this computer"))
                    .checked(state.map(|s| s.remember_ssh_credential))
                    .on_checked_change(move |checked| state.revise(|s| s.remember_ssh_credential = checked))
            }),
            text().class("subtle").content(i18n::bind(if ssh_credentials::SUPPORTED {
                "Remembering uses the system credential store and requires a saved SSH host fingerprint."
            } else {
                "凭据仅用于本次连接，不会保存到配置。"
            })),
        )),
        (
            cancel(ui),
            btn("连接", move || {
                begin_connect(&connect);
            })
            .class("button primary"),
        ),
    )
}

fn begin_connect(ui: &Ui) {
    if !ui
        .state
        .with(|s| s.dialog == Some(Dialog::Connect) && !s.connecting && !s.connected)
    {
        return;
    }
    ui.state.revise(|s| {
        s.dialog = None;
        s.dialog_error.clear();
    });
    ui.connect(true, false);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteKind {
    Server,
    DissolveGroup,
    GroupAndServers,
}

fn delete_dialog(ui: Ui, kind: DeleteKind) -> impl Widget {
    let state = ui.state;
    let group = kind != DeleteKind::Server;
    let target = state.with(|s| match &s.dialog {
        Some(Dialog::DeleteServer(id))
        | Some(Dialog::DissolveGroup(id))
        | Some(Dialog::DeleteGroupAndServers(id)) => id.clone(),
        _ => String::new(),
    });
    let name = state
        .with(|s| {
            if group {
                s.config
                    .groups
                    .iter()
                    .find(|g| g.id == target)
                    .map(|g| g.name.clone())
            } else {
                s.config
                    .available_servers()
                    .find(|p| p.id == target)
                    .map(|p| p.name.clone())
            }
        })
        .unwrap_or_default();
    let members = state.with(|s| {
        s.config
            .available_servers()
            .filter(|p| p.group_id.as_deref() == Some(&target))
            .map(|p| p.name.clone())
            .collect::<Vec<_>>()
    });
    let title = match kind {
        DeleteKind::Server => "删除服务器",
        DeleteKind::DissolveGroup => "解散分组",
        DeleteKind::GroupAndServers => "删除分组及服务器",
    };
    let details = match kind {
        DeleteKind::Server => i18n::translate("将删除此服务器的本地配置和已保存的 SSH 凭据。"),
        DeleteKind::DissolveGroup => {
            i18n::translate("只移除分组，所有服务器配置保留并移到根目录。")
        }
        DeleteKind::GroupAndServers => i18n::translate(&format!(
            "将删除此分组及其 {} 台服务器的本地配置和已保存的 SSH 凭据。",
            members.len()
        )),
    };
    let targets = if kind == DeleteKind::GroupAndServers {
        members.join("\n")
    } else {
        String::new()
    };
    let confirm = ui.clone();
    modal(
        ui.clone(),
        title,
        col().class("form-stack").fill((
            text()
                .class("dialog-description")
                .content(format!("{} “{name}”？", i18n::translate(title))),
            text().class("subtle").content(details),
            text()
                .class(if targets.is_empty() {
                    "hidden"
                } else {
                    "delete-targets"
                })
                .content(targets),
        )),
        (
            cancel(ui),
            btn(title, move || {
                let result = state.with(|s| -> anyhow::Result<AppConfig> {
                    let affects_active = kind != DeleteKind::DissolveGroup
                        && s.config.available_servers().any(|p| {
                            s.selected.as_deref() == Some(&p.id)
                                && if group {
                                    p.group_id.as_deref() == Some(&target)
                                } else {
                                    p.id == target
                                }
                        });
                    anyhow::ensure!(
                        !s.system_task_running || !affects_active,
                        "系统软件任务正在执行，完成后才能删除当前服务器"
                    );
                    let mut config = s.config.clone();
                    let removed = match kind {
                        DeleteKind::Server => config.delete_server(&target),
                        DeleteKind::DissolveGroup => config.delete_group(&target),
                        DeleteKind::GroupAndServers => config.delete_group_and_servers(&target),
                    };
                    anyhow::ensure!(removed, "目标已不存在，请关闭对话框后重试。");
                    persist(&confirm, &config)?;
                    Ok(config)
                });
                match result {
                    Ok(config) => {
                        confirm.forget_changed_credentials(&config);
                        let removed = state.with(|s| {
                            s.config
                                .servers
                                .iter()
                                .filter(|p| !config.servers.iter().any(|q| q.id == p.id))
                                .map(|p| p.id.clone())
                                .collect::<HashSet<_>>()
                        });
                        let affected = state
                            .with(|s| s.selected.as_ref().is_some_and(|id| removed.contains(id)));
                        if affected {
                            confirm.switch_session(None);
                        }
                        for id in &removed {
                            confirm.discard_parked(&Some(id.clone()));
                        }
                        state.revise(|s| {
                            s.config = config;
                            if group {
                                s.collapsed_groups.remove(&target);
                                if s.group.as_ref() == Some(&target) {
                                    s.group = None;
                                }
                            }
                            s.jobs.retain(|j| !removed.contains(&j.server_id));
                            s.dialog = None;
                            s.notice.clear();
                        });
                    }
                    Err(error) => state.revise(|s| s.dialog_error = format!("{error:#}")),
                }
            })
            .class(if kind == DeleteKind::DissolveGroup {
                "button primary"
            } else {
                "button danger"
            }),
        ),
    )
}

pub(crate) fn dialogs(ui: Ui) -> impl Widget {
    let state = ui.state;
    fragment((
        when(state.map(|s| s.dialog == Some(Dialog::Server)), {
            let ui = ui.clone();
            move || server_dialog(ui.clone())
        }),
        when(
            state.map(|s| matches!(s.dialog, Some(Dialog::Group { .. }))),
            {
                let ui = ui.clone();
                move || group_dialog(ui.clone())
            },
        ),
        when(
            state.map(|s| matches!(s.dialog, Some(Dialog::LocalDemo))),
            {
                let ui = ui.clone();
                move || local_demo::dialog(ui.clone())
            },
        ),
        when(state.map(|s| s.dialog == Some(Dialog::Import)), {
            let ui = ui.clone();
            move || import_dialog(ui.clone())
        }),
        when(state.map(|s| s.dialog == Some(Dialog::Connect)), {
            let ui = ui.clone();
            move || connect_dialog(ui.clone())
        }),
        when(
            state.map(|s| matches!(s.dialog, Some(Dialog::DeleteServer(_)))),
            {
                let ui = ui.clone();
                move || delete_dialog(ui.clone(), DeleteKind::Server)
            },
        ),
        when(
            state.map(|s| matches!(s.dialog, Some(Dialog::DissolveGroup(_)))),
            {
                let ui = ui.clone();
                move || delete_dialog(ui.clone(), DeleteKind::DissolveGroup)
            },
        ),
        when(
            state.map(|s| matches!(s.dialog, Some(Dialog::DeleteGroupAndServers(_)))),
            move || delete_dialog(ui.clone(), DeleteKind::GroupAndServers),
        ),
    ))
}
