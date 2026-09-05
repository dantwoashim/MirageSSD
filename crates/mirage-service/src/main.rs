#![cfg_attr(windows, allow(unsafe_code))]

#[cfg(windows)]
mod windows_service_host {
    use mirage_service::{ControlPlaneHandler, NativeMountControl, serve_one, wake_server};
    use std::{
        ffi::OsString,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    const SERVICE_NAME: &str = "MirageSSD";

    define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> Result<(), windows_service::Error> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(error) = run_service() {
            eprintln!("MirageSSD service failed: {error}");
        }
    }

    fn run_service() -> Result<(), Box<dyn std::error::Error>> {
        let stopping = Arc::new(AtomicBool::new(false));
        let stop_signal = Arc::clone(&stopping);
        let status =
            service_control_handler::register(SERVICE_NAME, move |control| match control {
                ServiceControl::Stop => {
                    stop_signal.store(true, Ordering::Release);
                    wake_server();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            })?;

        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 1,
            wait_hint: Duration::from_secs(30),
            process_id: None,
        })?;

        let program_data =
            std::env::var_os("ProgramData").ok_or("ProgramData is unavailable to the service")?;
        let state_root = std::path::PathBuf::from(program_data).join("MirageSSD");
        std::fs::create_dir_all(&state_root)?;
        let service_executable = std::env::current_exe()?;
        let filesystem_host = service_executable
            .parent()
            .ok_or("service executable has no parent")?
            .join("mirage-fs.exe");
        let handler = Arc::new(ControlPlaneHandler::with_mount_control(
            mirage_db::Database::open(&state_root.join("control.db"))?,
            NativeMountControl::new(filesystem_host),
        ));
        handler.recover_native_activations()?;
        handler.maintain_persistent_mounts()?;

        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            process_id: None,
        })?;
        let mount_watchdog_handler = Arc::clone(&handler);
        let mount_watchdog_stopping = Arc::clone(&stopping);
        let mount_watchdog = thread::spawn(move || {
            while !mount_watchdog_stopping.load(Ordering::Acquire) {
                if let Err(error) = mount_watchdog_handler.recover_native_activations() {
                    eprintln!("MirageSSD native activation recovery failed: {error}");
                }
                if let Err(error) = mount_watchdog_handler.maintain_persistent_mounts() {
                    eprintln!("MirageSSD persistent mount maintenance failed: {error}");
                }
                for _ in 0..20 {
                    if mount_watchdog_stopping.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        while !stopping.load(Ordering::Acquire) {
            let _ = serve_one(handler.as_ref());
        }
        let _ = mount_watchdog.join();
        drop(handler);

        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            process_id: None,
        })?;
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), windows_service::Error> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--uninstall-check")) {
        let safe = std::env::args_os().nth(2).is_some_and(|root| {
            mirage_db::check_uninstall_safety(&std::path::PathBuf::from(root).join("control.db"))
                .is_ok_and(|report| report.is_safe())
        });
        if !safe {
            eprintln!("MirageSSD uninstall blocked: restore or finish active state first.");
            std::process::exit(1);
        }
        return Ok(());
    }
    windows_service_host::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mirage-service requires Windows");
}
