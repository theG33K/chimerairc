//! ChimeraIRC GUI entry point.
//!
//! Native Win32 so the app keeps HydraIRC's classic look: MDI child windows, a tab bar,
//! menus and a status bar. Networking runs on a tokio runtime; events cross to the UI
//! thread through a queue plus a posted window message.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
mod dccrules;
#[cfg(windows)]
mod dccwin;
#[cfg(windows)]
mod dock;
#[cfg(windows)]
mod dialogs;
mod chanlist;
#[cfg(windows)]
mod favdialog;
mod favorites;
#[cfg(windows)]
mod chanwin;
mod highlight;
mod history;
#[cfg(windows)]
mod igndialog;
mod ignorelist;
mod logger;
#[cfg(windows)]
mod hldialog;
mod complete;
mod layouts;
mod servers;
mod buddies;
mod capsfmt;
mod about;
mod alias;
mod identity;
mod linkinfo;
mod paste;
mod logsearch;
mod typing;
mod columns;
#[cfg(windows)]
mod listcols;
mod winsizes;
mod stspolicy;
mod winplace;
mod urls;
mod perform;
mod rowfilter;
mod settings;
mod script;
mod sounds;
mod theme;
#[cfg(windows)]
mod look;
#[cfg(windows)]
mod ui;
mod util;

#[cfg(windows)]
fn run() -> windows::core::Result<()> {
    use windows::Win32::Graphics::Gdi::UpdateWindow;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // COM is needed for the standard open / save file dialogs.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    app::init();
    // A panic must end the process: unwinding would otherwise wait on runtime teardown.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("ChimeraIRC crashed: {info}");
        std::process::exit(101);
    }));
    let frame = ui::init()?;
    let accel = ui::create_accelerators()?;
    unsafe {
        ui::show_frame(frame);
        let _ = UpdateWindow(frame);
    }
    app::sync_menu();
    look::apply();
    app::update_check_on_start();
    app::new_server_window();
    app::scripts_start();
    app::refresh_status();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if TranslateMDISysAccel(ui::mdi_client(), &msg).as_bool() {
                continue;
            }
            if TranslateAcceleratorW(frame, accel, &msg) != 0 {
                continue;
            }
            if chanwin::is_dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    app::shutdown_runtime();
    Ok(())
}

fn main() {
    let _ = util::uptime_secs(); // starts the uptime clock
    #[cfg(windows)]
    if let Err(e) = run() {
        eprintln!("ChimeraIRC failed to start: {e}");
    }
}
