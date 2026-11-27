//! Shared models and SSH operations for the Arcnix client.

mod automation;
mod config;
mod connection;
mod desktop;
mod files;
mod maintenance;
mod observability;
mod software;

// Keep existing module paths available to the client and external callers.
pub use automation::{
    native_tasks, operations, remote_tasks, script_bundles, script_favorites, script_parameters,
    scripts, startup_services, target_selection, task_capabilities, task_records, task_result,
    task_stops, tasks,
};
pub use config::{model, preferences, server_labels, store};
use connection::private_program;
#[cfg(windows)]
pub use connection::wsl;
pub use connection::{
    browser_proxy, file_capabilities, host, local_desktop, private_keys, remote, ssh_config,
    terminal, transport_error,
};
use desktop::desktop_storage;
pub use desktop::{
    browser, desktop_customization, desktop_shortcuts, desktop_theme, desktop_time_zone,
    device_info, server_apps, settings_capabilities, sync,
};
pub use files::{deletion, file_associations, local, permissions, recycle, text_files, transfer};
pub use maintenance::{
    audit_cleanup, audit_details, audit_storage, backup_evidence, backups, cleanup_records,
    configuration_backups, configuration_compare, configuration_records, configuration_templates,
    configurations, database_backups, export_download, record_cleanup, record_maintenance,
    record_recovery, recovery_drills, snapshot_files,
};
pub use model::*;
pub use observability::{
    disks, gpu, host_capabilities, metric_history, metrics, network, processes, resource_ports,
    system_logs,
};
pub use software::{
    compose_apps, container_app, docker, docker_daemon, docker_pull, docker_registry,
    installation_plan, installers, nginx, nginx_config, nginx_site, packages, pgadmin, redis,
    server_inventory, software_capabilities, software_catalog, software_install, updates,
};
