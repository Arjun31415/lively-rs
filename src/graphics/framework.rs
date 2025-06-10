// taken from https://github.com/gfx-rs/wgpu/blob/trunk/examples/common/src/framework.rs
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::CompositorState,
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_seat,
    output::OutputState,
    registry::RegistryState,
    seat::SeatState,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface},
    },
};
use std::ptr::NonNull;
use std::thread;
use std::time;
use wayland_client::{Connection, Proxy, globals::registry_queue_init, protocol::wl_surface};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MouseUniform {
    pub pos: [f32; 2],
}
#[allow(dead_code)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
}

pub struct Wallpaper {
    pub registry_state: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub exit: bool,
    pub first_configure: bool,
    pub width: u32,
    pub height: u32,
    pub adapter: wgpu::Adapter,
    pub queue: wgpu::Queue,
    pub device: wgpu::Device,
    pub surface: wgpu::Surface<'static>,
    pub wl_surface: wl_surface::WlSurface,
    pub mouse_pos_rx: std::sync::mpsc::Receiver<(i64, i64)>,
    pub mouse_buf: wgpu::Buffer,
    pub mouse_bind_group: wgpu::BindGroup,
    pub mouse_bind_group_layout: wgpu::BindGroupLayout,
    // pub shift: Option<u32>,
    pub layer: LayerSurface,
}
pub trait WgpuConfig: 'static + Sized {
    fn optional_features() -> wgpu::Features {
        wgpu::Features::empty()
    }
    fn required_features() -> wgpu::Features {
        wgpu::Features::empty()
    }
    fn required_downlevel_capabilities() -> wgpu::DownlevelCapabilities {
        wgpu::DownlevelCapabilities {
            flags: wgpu::DownlevelFlags::empty(),
            shader_model: wgpu::ShaderModel::Sm5,
            ..wgpu::DownlevelCapabilities::default()
        }
    }
    fn required_limits() -> wgpu::Limits {
        wgpu::Limits::downlevel_webgl2_defaults() // These downlevel limits will allow the code to run on all possible hardware
    }
}

pub async fn setup<E: WgpuConfig>() {
    env_logger::init();
    // All Wayland apps start by connecting the compositor (server).
    let conn = Connection::connect_to_env().unwrap();

    // Enumerate the list of globals to get the protocols the server implements.
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    // The compositor (not to be confused with the server which is commonly called the compositor) allows
    // configuring surfaces to be presented.
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor is not available");
    let surface = compositor.create_surface(&qh);
    // This app uses the wlr layer shell, which may not be available with every compositor.
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell is not available");
    // Initialize wgpu
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    log::info!("Initializing layer_shell");
    // And then we create the layer shell.
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Background,
        Some("simple_layer"),
        None,
    );
    let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        NonNull::new(conn.backend().display_ptr() as *mut _).unwrap(),
    ));
    let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
        NonNull::new(layer.wl_surface().id().as_ptr() as *mut _).unwrap(),
    ));

    let surface = unsafe {
        instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle,
                raw_window_handle,
            })
            .unwrap()
    };

    // Pick a supported adapter
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("Failed to find suitable adapter");
    let (_device, _queue) = pollster::block_on(adapter.request_device(&Default::default()))
        .expect("Failed to request device");
    // Configure the layer surface, providing things like the anchor on screen, desired size and the keyboard
    // interactivity
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT | Anchor::LEFT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.set_exclusive_zone(-1);
    layer.commit();
    let wl_surface = layer.wl_surface().clone();

    let adapter_info = adapter.get_info();
    println!("Using {} ({:?})", adapter_info.name, adapter_info.backend);

    let optional_features = E::optional_features();
    let required_features = E::required_features();
    let adapter_features = adapter.features();
    assert!(
        adapter_features.contains(required_features),
        "Adapter does not support required features for this example: {:?}",
        required_features - adapter_features
    );

    let required_downlevel_capabilities = E::required_downlevel_capabilities();
    let downlevel_capabilities = adapter.get_downlevel_capabilities();
    assert!(
        downlevel_capabilities.shader_model >= required_downlevel_capabilities.shader_model,
        "Adapter does not support the minimum shader model required to run this example: {:?}",
        required_downlevel_capabilities.shader_model
    );
    assert!(
        downlevel_capabilities
            .flags
            .contains(required_downlevel_capabilities.flags),
        "Adapter does not support the downlevel capabilities required to run this example: {:?}",
        required_downlevel_capabilities.flags - downlevel_capabilities.flags
    );

    // Make sure we use the texture resolution limits from the adapter, so we can support images the size of the surface.
    let needed_limits = E::required_limits().using_resolution(adapter.limits());

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                required_features: (optional_features & adapter_features) | required_features,
                required_limits: needed_limits,
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
            },
        )
        .await
        .expect("Unable to find a suitable GPU adapter!");
    let mouse_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Mouse Uniform Buffer"),
        size: std::mem::size_of::<MouseUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mouse_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mouse Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

    let mouse_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Mouse Bind Group"),
        layout: &mouse_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: mouse_buf.as_entire_binding(),
        }],
    });
    let (tx, rx) = std::sync::mpsc::channel::<(i64, i64)>();

    let tx_clone = tx.clone();
    let mut w = Wallpaper {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        exit: false,
        first_configure: true,
        width: 256,
        height: 256,
        device,
        wl_surface,
        surface,
        adapter,
        queue,
        layer,
        mouse_pos_rx: rx,
        mouse_buf: mouse_buf,
        mouse_bind_group: mouse_bind_group,
        mouse_bind_group_layout: mouse_bind_group_layout,
    };
    let handle = thread::spawn(move || {
        use std::process;
        println!("My pid is {}", process::id());
        track_mouse_movement(tx_clone);
        println!("Thread over");
    });
    println!("Starting event loop");

    loop {
        event_queue.blocking_dispatch(&mut w).unwrap();

        if w.exit {
            log::info!("Exiting");
            // TODO: destroy the thread handle
            break;
        }
    }
    handle.join().unwrap();
}
fn track_mouse_movement(tx: std::sync::mpsc::Sender<(i64, i64)>) {
    let mut last_pos = (-1, -1);
    loop {
        let cursor_pos =
            <hyprland::data::CursorPosition as hyprland::shared::HyprData>::get().unwrap();
        if last_pos != (cursor_pos.x, cursor_pos.y) {
            last_pos = (cursor_pos.x, cursor_pos.y);
            tx.send((cursor_pos.x, cursor_pos.y))
                .expect("send should succeed");

            let ten_millis = time::Duration::from_millis(25);
            thread::sleep(ten_millis);
        }
    }
}
delegate_compositor!(Wallpaper);
delegate_output!(Wallpaper);
delegate_seat!(Wallpaper);
delegate_layer!(Wallpaper);

delegate_registry!(Wallpaper);
