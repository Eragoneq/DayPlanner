#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod assets;
mod block_editor;
mod calendar;
mod clock;
mod db;
mod model;
mod sync_panel;
mod task_editor;
mod theme;
mod timer;
mod todoist;
mod ui;

use gpui_kit::{self as gpui, *};

fn main() {
    let demo = std::env::args_os()
        .skip(1)
        .any(|argument| argument == "--demo");
    gpui::application()
        .with_assets(assets::PlannerAssets)
        .run(move |cx| {
            gpui::init(cx);
            task_editor::init(cx);
            sync_panel::init(cx);
            theme::init(cx);

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let (_, planner) = gpui::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1100.), px(700.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(
                            if demo {
                                "DayPlanner · Demo"
                            } else {
                                "DayPlanner"
                            }
                            .into(),
                        ),
                        appears_transparent: cfg!(target_os = "windows"),
                        ..Default::default()
                    }),
                    app_id: Some("dev.dayplanner.DayPlanner".into()),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let planner = cx.new(|cx| ui::DayPlanner::new(demo, cx));
                    let weak = planner.downgrade();
                    window.on_window_should_close(cx, move |window, cx| {
                        weak.update(cx, |planner, cx| planner.should_close(window, cx))
                            .unwrap_or(true)
                    });
                    planner
                },
            )
            .expect("Could not open the DayPlanner window");

            cx.bind_keys([
                KeyBinding::new("ctrl-t", ui::NewTask, None),
                // Let native text entry handle Space instead of the enclosing
                // popover's Space-to-Confirm shortcut, which toggles it closed.
                KeyBinding::new("space", NoAction, Some("Input")),
            ]);
            let keyboard_planner = planner.downgrade();
            cx.on_action(move |_: &ui::NewTask, cx| {
                let planner = keyboard_planner.clone();
                // Keyboard actions run while the window is borrowed by GPUI.
                cx.defer(move |cx| {
                    if let Some(window) = cx.active_window() {
                        let _ = window.update(cx, |_, window, cx| {
                            let _ = planner
                                .update(cx, |planner, cx| planner.new_task(None, window, cx));
                        });
                    }
                });
            });

            cx.on_app_quit(move |cx| {
                let ready = planner.read(cx).prepare_quit();
                async move {
                    if let Some(ready) = ready {
                        let _ = ready.recv().await;
                    }
                }
            })
            .detach();

            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
