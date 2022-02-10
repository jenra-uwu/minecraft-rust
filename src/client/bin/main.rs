use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use glium::glutin::dpi::PhysicalPosition;
use glium::glutin::event::{ElementState, VirtualKeyCode};
use glium::glutin::{
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
    ContextBuilder,
};
use glium::{Display, Program, Surface};
use minecraft_rust::client::light::LightSource;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use minecraft_rust::client::camera::Camera;
use minecraft_rust::client::chunk::{Chunk, ChunkWaiter, Mesh, BlockTextures};
use minecraft_rust::packet::{ServerPacket, UserPacket};
use minecraft_rust::client::player::Player;

const USERNAME: &str = "owo";
const ADDRESS: &str = "0.0.0.0:6429";

const VERTEX_SHADER: &str = include_str!("../shaders/vertex.glsl");
const FRAGMENT_SHADER: &str = include_str!("../shaders/fragment.glsl");

fn main() {
    let (send_tx, send_rx) = mpsc::channel(128);
    let (recv_tx, recv_rx) = mpsc::channel(128);
    let send_tx2 = send_tx.clone();
    thread::spawn(|| networking_loop(send_tx2, send_rx, recv_tx));
    main_loop(send_tx, recv_rx);
}

fn main_loop(tx: mpsc::Sender<UserPacket>, mut rx: mpsc::Receiver<ServerPacket>) {
    let event_loop = EventLoop::new();
    let wb = WindowBuilder::new();
    let cb = ContextBuilder::new().with_depth_buffer(24);
    let display = Display::new(wb, cb, &event_loop).unwrap();
    let mut locked = true;

    {
        let gl_window = display.gl_window();
        let window = gl_window.window();
        window.set_cursor_grab(true).unwrap();
        let size = window.inner_size();
        let centre = PhysicalPosition::new(size.width / 2, size.height / 2);
        window.set_cursor_position(centre).unwrap();
        window.set_cursor_visible(false);
    }

    let program = Program::from_source(&display, VERTEX_SHADER, FRAGMENT_SHADER, None).unwrap();

    let params = glium::DrawParameters {
        depth: glium::Depth {
            test: glium::draw_parameters::DepthTest::IfLess,
            write: true,
            ..Default::default()
        },
        backface_culling: glium::draw_parameters::BackfaceCullingMode::CullClockwise,
        ..Default::default()
    };

    let mut camera = Camera::new(50.0, 0.01, 90.0);
    let mut chunks = HashMap::new();
    let mut lights = vec![LightSource::new(15, 0, 0, 15, camera.get_pos())];
    let mut players = HashMap::new();
    let square = Mesh::square(&display);
    let block_textures = BlockTextures::generate_textures(&display);

    for x in -5..=5 {
        for y in -5..=5 {
            for z in -5..=5 {
                chunks.insert((x, y, z), ChunkWaiter::Timestamp(0));
            }
        }
    }

    let mut last = Instant::now();
    event_loop.run(move |event, _, control_flow| {
        match event {
            Event::WindowEvent { event, .. } => {
                match event {
                    WindowEvent::CloseRequested => {
                        *control_flow = ControlFlow::Exit;
                        tx.blocking_send(UserPacket::Disconnect).unwrap();
                    }

                    WindowEvent::Focused(false) => {
                        let gl_window = display.gl_window();
                        let window = gl_window.window();
                        window.set_cursor_visible(true);
                        window.set_cursor_grab(false).unwrap();
                        locked = false;
                    }

                    WindowEvent::KeyboardInput { input, .. } if matches!(input.virtual_keycode, Some(VirtualKeyCode::Escape)) && matches!(input.state, ElementState::Released) => {
                        let gl_window = display.gl_window();
                        let window = gl_window.window();
                        let size = window.inner_size();
                        let centre = PhysicalPosition::new(size.width / 2, size.height / 2);
                        window.set_cursor_position(centre).unwrap();
                        window.set_cursor_visible(locked);
                        locked ^= true;
                        window.set_cursor_grab(locked).unwrap();
                    }

                    WindowEvent::KeyboardInput { input, .. } if locked && camera.move_self(input) => {
                        //lights.get_mut(0).unwrap().set_location(camera.get_pos());
                    }

                    WindowEvent::CursorMoved { position, .. } if locked => {
                        let gl_window = display.gl_window();
                        let window = gl_window.window();
                        let size = window.inner_size();
                        let centre = PhysicalPosition::new(size.width / 2, size.height / 2);
                        camera.turn_self(
                            position.x as i32 - centre.x as i32,
                            position.y as i32 - centre.y as i32,
                        );
                        window.set_cursor_position(centre).unwrap();
                    }

                    _ => (),
                }

                return;
            }

            Event::NewEvents(cause) => match cause {
                StartCause::Init => {
                    last = Instant::now();
                    let next_frame_time = last + Duration::from_nanos(16_666_667);
                    *control_flow = ControlFlow::WaitUntil(next_frame_time);
                    return;
                }

                StartCause::ResumeTimeReached { .. } => (),

                _ => return,
            },

            _ => return,
        }

        let delta = Instant::now() - last;
        let mut keys: Vec<(i32, i32, i32)> = Vec::new();

        while let Ok(packet) = rx.try_recv() {
            match packet {
                ServerPacket::ConnectionAccepted => (),
                ServerPacket::Disconnected { .. } => (),
                ServerPacket::Pong { .. } => (),

                ServerPacket::UserJoin { name, pos } => {
                    players.insert(name.clone(), Player::new(name, pos, &display));
                }

                ServerPacket::UserLeave { name } => {
                    players.remove(&name);
                }

                ServerPacket::MoveUser { name, pos } => {
                    if let Some(player) = players.get_mut(&name) {
                        player.position = pos;
                    }
                }

                ServerPacket::NewChunk { chunk } => {
                    let coords = (chunk.get_chunk_x(), chunk.get_chunk_y(), chunk.get_chunk_z());
                    chunks.insert(coords, ChunkWaiter::Chunk(Chunk::from_server_chunk(chunk)));
                    let (x, y, z) = coords;

                    if let Some(ChunkWaiter::Chunk(chunk)) = chunks.get_mut(&(x - 1, y, z)) {
                        chunk.invalidate_mesh();
                    }
                    if let Some(ChunkWaiter::Chunk(chunk)) = chunks.get_mut(&(x + 1, y, z)) {
                        chunk.invalidate_mesh();
                    }
                    if let Some(ChunkWaiter::Chunk(chunk)) = chunks.get_mut(&(x, y - 1, z)) {
                        chunk.invalidate_mesh();
                    }
                    if let Some(ChunkWaiter::Chunk(chunk)) = chunks.get_mut(&(x, y + 1, z)) {
                        chunk.invalidate_mesh();
                    }
                    if let Some(ChunkWaiter::Chunk(chunk)) = chunks.get_mut(&(x, y, z - 1)) {
                        chunk.invalidate_mesh();
                    }
                    if let Some(ChunkWaiter::Chunk(chunk)) = chunks.get_mut(&(x, y, z + 1)) {
                        chunk.invalidate_mesh();
                    }
                }
            }
        }

        camera.tick(delta);
        if camera.is_moving() {
            let _ = tx.try_send(UserPacket::MoveSelf { pos: camera.get_pos() });
        }

        let mut target = display.draw();
        target.clear_color_and_depth((0.53, 0.80, 0.92, 1.0), 1.0);

        let perspective = camera.perspective(&target);

        let view = camera.view_matrix();

        let mut changed = 0;
        keys.extend(chunks.keys());
        let lights_changed = lights.iter().any(LightSource::updated);
        for key in keys.iter() {
            let mut chunk = chunks.remove(key).unwrap();
            if let ChunkWaiter::Chunk(chunk) = &mut chunk {
                if chunk.generate_mesh(&display, &chunks) {
                    changed += 1;
                    chunk.invalidate_lights();
                    chunk.populate_lights(&display, &lights);
                } else if lights_changed {
                    chunk.invalidate_lights();
                    chunk.populate_lights(&display, &lights);
                }
                chunk.render(&mut target, &program, perspective, view, &params, &square, &block_textures);
            }
            chunks.insert(*key, chunk);

            if changed > 50 {
                break;
            }
        }

        if lights_changed {
            for light in lights.iter_mut() {
                light.reset_updated();
            }
        }

        keys.clear();

        let timestamp = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        for ((x, y, z), chunk) in chunks.iter_mut() {
            if let Some(stamp) = chunk.timestamp() {
                if timestamp - stamp > 100_000 {
                    *chunk = ChunkWaiter::Timestamp(timestamp);
                    let _ = tx.try_send(UserPacket::RequestChunk { x: *x, y: *y, z: *z });
                }
            }
        }

        for (_, player) in players.iter() {
            player.render(&mut target, &program, perspective, view, &params);
        }


        target.finish().unwrap();

        last = Instant::now();
        let next_frame_time = last + Duration::from_nanos(16_666_667);
        *control_flow = ControlFlow::WaitUntil(next_frame_time);
    });
}

#[tokio::main]
async fn networking_loop(tx: mpsc::Sender<UserPacket>, rx: mpsc::Receiver<UserPacket>, recv_tx: mpsc::Sender<ServerPacket>) -> std::io::Result<()> {
    let sock = Arc::new(UdpSocket::bind(ADDRESS).await.unwrap());
    sock.connect("127.0.0.1:6942").await?;

    tokio::spawn(transmitting(rx, sock.clone()));
    receiving(tx, sock, recv_tx).await
}

async fn transmitting(mut rx: mpsc::Receiver<UserPacket>, sock: Arc<UdpSocket>) -> std::io::Result<()> {
    sock.send(&bincode::serialize(&UserPacket::ConnectionRequest { name: String::from(USERNAME), }).unwrap()).await?;

    while let Some(packet) = rx.recv().await {
        sock.send(&bincode::serialize(&packet).unwrap()).await?;
    }

    Ok(())
}

async fn receiving(tx: mpsc::Sender<UserPacket>, sock: Arc<UdpSocket>, recv_tx: mpsc::Sender<ServerPacket>) -> std::io::Result<()> {
    let mut buf = Box::new([0; 2usize.pow(20)]);
    loop {
        let len = sock.recv(&mut *buf).await?;
        let packet: ServerPacket = bincode::deserialize(&buf[..len]).unwrap();

        match packet {
            ServerPacket::ConnectionAccepted => {
                println!("Connected to server!");
                tokio::spawn(ping(tx.clone()));
            }

            ServerPacket::Disconnected { reason } => {
                println!("Disconnected from server for reason {}", reason);
                break Ok(());
            }

            ServerPacket::Pong { timestamp } => {
                let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
                let duration = now - timestamp;
                let duration = Duration::from_nanos(duration as u64);
                println!("Pong! {:?}", duration);
            }

            ServerPacket::UserJoin { name, pos } => {
                println!("{} joined the game", name);
                recv_tx.send(ServerPacket::UserJoin { name, pos }).await.unwrap();
            }

            ServerPacket::UserLeave { name } => {
                println!("{} left the game", name);
                recv_tx.send(ServerPacket::UserLeave { name }).await.unwrap();
            }

            ServerPacket::MoveUser { name, pos } => {
                recv_tx.send(ServerPacket::MoveUser { name, pos }).await.unwrap();
            }

            ServerPacket::NewChunk { chunk } => {
                recv_tx.send(ServerPacket::NewChunk { chunk }).await.unwrap();
            }
        }
    }
}

async fn ping(tx: mpsc::Sender<UserPacket>) {
    loop {
        let timestamp = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        tx.send(UserPacket::Ping{ timestamp }).await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
