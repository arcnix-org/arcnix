pub(crate) mod app_center_ui;
pub(crate) mod compose_ui;
pub(crate) mod docker_ui;
pub(crate) mod installation_ui;
pub(crate) mod nginx_ui;
mod package_browser;
pub(crate) mod pgadmin_native;
pub(crate) mod pgadmin_ui;
pub(crate) mod redis_ui;
pub(crate) mod software_install_ui;

pub(crate) use package_browser::{
    Event, SoftwareState, event, on_connection_lost, packages, server_reply,
};
