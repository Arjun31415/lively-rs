use crate::Cli;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use webkit6::gdk;
use webkit6::gio;
use webkit6::glib;
use webkit6::prelude::*;
use webkit6::{
    HardwareAccelerationPolicy, Settings, UserContentInjectedFrames, UserContentManager,
    UserScript, UserScriptInjectionTime, WebView,
};

const POINTER_HOOK_JS: &str = r#"
(function() {
  let canvas = null;
  function getCanvas() {
    if (!canvas) canvas = document.querySelector('canvas');
    return canvas;
  }

  window.__setPointer = function(x, y) {
    const el = getCanvas();
    if (!el) return;
    const opts = {
      clientX: x, clientY: y, bubbles: true, cancelable: true,
      composed: true, pointerId: 1, pointerType: 'mouse', isPrimary: true,
    };
    el.dispatchEvent(new PointerEvent('pointermove', opts));
    el.dispatchEvent(new MouseEvent('mousemove', opts));
  };
})();
"#;

pub fn run(args: Cli) {
    let app = gtk4::Application::builder()
        .application_id("dev.example.rustwallpaper")
        .build();

    let args_clone = Arc::new(args);
    app.connect_activate(move |app| {
        build_ui(app, &args_clone);
    });

    app.run_with_args::<&str>(&[]);
}

fn build_ui(app: &gtk4::Application, args: &Cli) {
    let window = gtk4::ApplicationWindow::new(app);
    window.set_title(Some("web-wallpaper"));

    window.init_layer_shell();
    window.set_namespace(Some("rust-wallpaper"));
    window.set_layer(Layer::Background);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);

    let mut monitor_offset = (0.0f64, 0.0f64);

    if let Some(ref target_mon) = args.monitor {
        let display = gdk::Display::default().expect("No display found");
        let monitors = display.monitors();
        for i in 0..monitors.n_items() {
            if let Some(mon) = monitors.item(i).and_downcast::<gdk::Monitor>()
                && mon.connector().as_deref() == Some(target_mon)
            {
                window.set_monitor(Some(&mon));
                let geo = mon.geometry();
                monitor_offset = (geo.x() as f64, geo.y() as f64);
                break;
            }
        }
    }

    let settings = Settings::new();
    settings.set_enable_webgl(true);
    settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Always);
    if args.debug {
        settings.set_enable_developer_extras(true);
    }

    let content_manager = UserContentManager::new();
    let user_script = UserScript::new(
        POINTER_HOOK_JS,
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    );
    content_manager.add_script(&user_script);

    let webview = WebView::builder()
        .settings(&settings)
        .user_content_manager(&content_manager)
        .build();

    window.set_child(Some(&webview));

    if args.debug
        && let Some(inspector) = webview.inspector()
    {
        webview.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                inspector.show();
            }
        });
    }
    if args.audio {
        let webview_for_audio_setup = webview.clone();
        webview.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished {
                webview_for_audio_setup.evaluate_javascript(
                    "window.livelyPropertyListener && window.livelyPropertyListener('audioReact', true);",
                    None,
                    None,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
        });
    }
    let html_path = args.wallpaper_path.join("index.html");
    let abs_html = fs::canonicalize(&html_path).unwrap_or_else(|_| {
        panic!("HTML file not found at: {:?}", html_path);
    });
    let uri = format!("file://{}", abs_html.to_string_lossy());
    webview.load_uri(&uri);

    let (tx, rx) = std::sync::mpsc::channel::<(i64, i64)>();
    let spectrum = if args.audio {
        let spectrum = crate::audio::new_spectrum_handle();
        crate::audio::start_audio_tracking(spectrum.clone());
        Some(spectrum)
    } else {
        None
    };

    crate::mouse::start_mouse_tracking(args.monitor.clone(), tx);
    let mut last_pos = (0.0f64, 0.0f64);
    let debug = args.debug;

    glib::timeout_add_local(Duration::from_millis(33), move || {
        while let Ok((x, y)) = rx.try_recv() {
            last_pos = (x as f64, y as f64);
        }

        let (x, y) = last_pos;
        let mut script = format!("window.__setPointer && window.__setPointer({x}, {y});");

        if let Some(ref spectrum) = spectrum {
            // Always reads whatever the audio thread most recently wrote —
            // never stale, never backlogged.
            let bins = spectrum.lock().unwrap();
            if debug {
                println!("Audio level (bin 0): {}", bins[0]);
            }
            let audio_array = format!(
                "[{}]",
                bins.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            script.push_str(&format!(
                "window.livelyAudioListener && window.livelyAudioListener({audio_array});"
            ));
        }

        webview.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
        glib::ControlFlow::Continue
    });

    window.present();
}
