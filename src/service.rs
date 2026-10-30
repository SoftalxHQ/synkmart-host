//! Windows service entry points. On non-Windows builds this module is a stub.

#[cfg(windows)]
pub mod windows {
    use crate::config::Config;
    use std::{ffi::OsString, time::Duration};
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    const SERVICE_NAME: &str = "SynkMartHost";

    define_windows_service!(ffi_service_main, service_main);

    pub fn run_as_service() -> windows_service::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            tracing::error!(error = %e, "windows service failed");
        }
    }

    fn run_service() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
        let status_handle = service_control_handler::register(SERVICE_NAME, move |event| {
            match event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = shutdown_tx.send(());
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        })?;

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        use clap::Parser;
        let config = Config::parse();
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let handle = tokio::spawn(crate::run_server(config));
            let _ = tokio::task::spawn_blocking(move || shutdown_rx.recv()).await;
            handle.abort();
        });

        status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;
        Ok(())
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn run_as_service() -> anyhow::Result<()> {
    anyhow::bail!("Windows service mode is only available on Windows")
}
