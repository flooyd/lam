// #![windows_subsystem = "windows"]

use ::rand::thread_rng;
use ::rand::Rng;
use bincode;
use macroquad::prelude::*;
use message_io::network::{NetEvent, Transport};
use message_io::node::{self, NodeEvent};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

#[derive(Serialize, Deserialize, Debug)]
enum ClientMessage {
    PlayerPosition { id: usize, x: f32, y: f32 },
    AssignPlayerId { id: usize },
    UpdateMessage { id: usize, message: String },
    OtherPlayerDisconnected { id: usize },
}

#[derive(Clone)]
struct Player {
    id: usize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    speed: f32,
    target_x: Option<f32>,
    target_y: Option<f32>,
    last_message_send_time: Instant,
    message: Option<String>,
    message_sent: bool,
    position_changed: bool,
    current_pose_index: usize,
    next_pose_index: usize,
    last_pose_update_time: Instant,
    pose_update_interval: Duration,
    pose_interp_factor: f32,
    hair_lines: Vec<((f32, f32), (f32, f32))>,
    is_local: bool,
    is_moving: bool,     // Tracks if the player is currently moving
    bobbing_time: f32,   // Time accumulator for bobbing
    bobbing_offset: f32, // Current y-offset for bobbing
}

impl Player {
    fn new_local(x: f32, y: f32, width: f32, height: f32) -> Self {
        let mut player = Player {
            id: 0, // Will be set by the server
            x,
            y,
            width,
            height,
            speed: 250.0,
            target_x: None,
            target_y: None,
            last_message_send_time: Instant::now(),
            message: None,
            message_sent: false,
            position_changed: false,
            current_pose_index: 0,
            next_pose_index: 1,
            last_pose_update_time: Instant::now(),
            pose_update_interval: Duration::from_millis(100), // 20 updates per second
            pose_interp_factor: 0.0,
            hair_lines: Vec::new(),
            is_local: true,
            is_moving: false,
            bobbing_time: 0.0,
            bobbing_offset: 0.0,
        };
        player.generate_hair();
        player
    }

    fn new_other(id: usize, x: f32, y: f32) -> Self {
        let mut player = Player {
            id,
            x,
            y,
            width: 30.0, // Default values for other players
            height: 30.0,
            speed: 250.0,
            target_x: Some(x),
            target_y: Some(y),
            last_message_send_time: Instant::now(),
            message: None,
            message_sent: true, // Other players don't send messages
            position_changed: false,
            current_pose_index: 0,
            next_pose_index: 1,
            last_pose_update_time: Instant::now(),
            pose_update_interval: Duration::from_millis(100),
            pose_interp_factor: 0.0,
            hair_lines: Vec::new(),
            is_local: false,
            is_moving: false,
            bobbing_time: 0.0,
            bobbing_offset: 0.0,
        };
        player.generate_hair();
        player
    }

    fn generate_hair(&mut self) {
        let mut hair_lines = Vec::with_capacity(250);
        let hair_count = 250;
        let base_hair_length = 20.0;

        let mut rng = thread_rng();

        for _ in 0..hair_count {
            let angle = rng.gen_range(-180.0_f32.to_radians()..180.0_f32.to_radians());
            let angle_variation = rng.gen_range(-5.0_f32.to_radians()..5.0_f32.to_radians());

            let start_x = 15.0 * angle.cos();
            let start_y = -30.0 + rng.gen_range(0.0..10.0);

            let hair_length = base_hair_length + rng.gen_range(-5.0..5.0);

            let end_x = start_x + hair_length * (angle + angle_variation).cos();
            let end_y =
                start_y + hair_length * (angle + angle_variation).sin() + rng.gen_range(0.0..5.0);

            hair_lines.push(((start_x, start_y), (end_x, end_y)));
        }

        self.hair_lines = hair_lines;
    }

    fn update(&mut self, dt: f32) {
        // Move towards target position at a constant speed
        if let (Some(target_x), Some(target_y)) = (self.target_x, self.target_y) {
            let direction = Vec2::new(target_x - self.x, target_y - self.y);
            let distance = direction.length();

            if distance < self.speed * dt {
                // Close enough to the target
                self.x = target_x;
                self.y = target_y;
                self.target_x = None;
                self.target_y = None;
                self.is_moving = false;
            } else {
                let direction = direction.normalize();
                self.x += direction.x * self.speed * dt;
                self.y += direction.y * self.speed * dt;
                self.is_moving = true;
            }
            self.position_changed = true;
        }

        // Clear message after 15 seconds
        if self.last_message_send_time.elapsed() >= Duration::from_secs(15) {
            self.message = None;
            self.last_message_send_time = Instant::now();
        }

        // Update pose
        let now = Instant::now();
        if self.is_moving {
            if now.duration_since(self.last_pose_update_time) >= self.pose_update_interval {
                self.current_pose_index = self.next_pose_index;
                self.next_pose_index = (self.next_pose_index + 1) % RUN_POSES.len();
                self.pose_interp_factor = 0.0;
                self.last_pose_update_time = now;
            } else {
                self.pose_interp_factor +=
                    1.0 / (self.pose_update_interval.as_secs_f32() * get_fps() as f32); // Assuming 60 FPS
                if self.pose_interp_factor > 1.0 {
                    self.pose_interp_factor = 1.0;
                }
            }

            // Update bobbing when moving
            self.bobbing_time += dt * 1.0; // Adjust speed as needed
            self.bobbing_offset = (self.bobbing_time * 5.0).sin() * 5.0; // amplitude of 5.0
        } else {
            // Reset bobbing when not moving
            self.bobbing_time = 0.0;
            self.bobbing_offset = 0.0;
        }
    }

    fn get_current_pose(&self) -> Pose {
        if self.is_moving {
            let start_pose = &RUN_POSES[self.current_pose_index];
            let end_pose = &RUN_POSES[self.next_pose_index];
            lerp_pose(start_pose, end_pose, self.pose_interp_factor)
        } else {
            IDLE_POSE
        }
    }

    fn draw(&self) {
        // Apply bobbing offset
        let y_offset = self.bobbing_offset;

        // Draw hair
        for line in &self.hair_lines {
            draw_line(
                self.x + line.0 .0,            // Start x (translated)
                self.y + line.0 .1 + y_offset, // Start y (translated with bobbing)
                self.x + line.1 .0,            // End x (translated)
                self.y + line.1 .1 + y_offset, // End y (translated with bobbing)
                1.0,                           // Thickness of hair strands
                BROWN,                         // Color of hair
            );
        }

        // Determine color based on whether it's the local player
        let body_color = if self.is_local { RED } else { BLACK };

        // Draw head
        draw_circle(self.x, self.y + y_offset, 20.0, body_color);

        // Draw eyes
        let eye_color = WHITE;
        draw_circle(self.x - 7.0, self.y - 5.0 + y_offset, 3.0, eye_color);
        draw_circle(self.x + 7.0, self.y - 5.0 + y_offset, 3.0, eye_color);

        // Draw mouth
        let mouth_color = WHITE;
        draw_line(
            self.x - 7.0,
            self.y + 5.0 + y_offset,
            self.x,
            self.y + 10.0 + y_offset,
            2.0,
            mouth_color,
        );
        draw_line(
            self.x,
            self.y + 10.0 + y_offset,
            self.x + 7.0,
            self.y + 5.0 + y_offset,
            2.0,
            mouth_color,
        );

        // Draw body
        draw_line(
            self.x,
            self.y + 10.0 + y_offset,
            self.x,
            self.y + 40.0 + y_offset,
            2.0,
            body_color,
        );

        // Get interpolated pose
        let pose = self.get_current_pose();

        // Draw arms
        draw_line(
            self.x,
            self.y + 20.0 + y_offset,
            self.x + pose.left_arm.0,
            self.y + pose.left_arm.1 + y_offset,
            2.0,
            body_color,
        );
        draw_line(
            self.x,
            self.y + 20.0 + y_offset,
            self.x + pose.right_arm.0,
            self.y + pose.right_arm.1 + y_offset,
            2.0,
            body_color,
        );

        // Draw legs
        draw_line(
            self.x,
            self.y + 40.0 + y_offset,
            self.x + pose.left_leg.0,
            self.y + pose.left_leg.1 + y_offset,
            2.0,
            body_color,
        );
        draw_line(
            self.x,
            self.y + 40.0 + y_offset,
            self.x + pose.right_leg.0,
            self.y + pose.right_leg.1 + y_offset,
            2.0,
            body_color,
        );

        // Draw message
        if let Some(message) = &self.message {
            // Draw black rectangle centered above player
            draw_rectangle(
                self.x - 75.0,
                self.y - 70.0 + y_offset,
                150.0,
                50.0,
                Color::new(0.0, 0.0, 0.0, 0.8),
            );
            draw_text(
                message,
                self.x - 50.0,
                self.y - 35.0 + y_offset,
                20.0,
                WHITE,
            );
        }
    }
}

struct Pose {
    left_arm: (f32, f32),
    right_arm: (f32, f32),
    left_leg: (f32, f32),
    right_leg: (f32, f32),
}

const RUN_POSES: [Pose; 5] = [
    // Pose 1: Right leg forward, left arm forward
    Pose {
        left_arm: (-20.0, 30.0),
        right_arm: (20.0, 30.0),
        left_leg: (-10.0, 60.0),
        right_leg: (15.0, 60.0),
    },
    // Pose 2: Both legs mid-motion, arms slightly bent
    Pose {
        left_arm: (-15.0, 30.0),
        right_arm: (15.0, 30.0),
        left_leg: (-5.0, 60.0),
        right_leg: (10.0, 60.0),
    },
    // Pose 3: Left leg forward, right arm forward
    Pose {
        left_arm: (-20.0, 30.0),
        right_arm: (20.0, 30.0),
        left_leg: (15.0, 60.0),
        right_leg: (-10.0, 60.0),
    },
    // Pose 4: Both legs mid-motion opposite to Pose 2
    Pose {
        left_arm: (-15.0, 30.0),
        right_arm: (15.0, 30.0),
        left_leg: (10.0, 60.0),
        right_leg: (-5.0, 60.0),
    },
    // Pose 5: Neutral pose
    Pose {
        left_arm: (-20.0, 30.0),
        right_arm: (20.0, 30.0),
        left_leg: (-10.0, 60.0),
        right_leg: (10.0, 60.0),
    },
];

fn lerp_pose(start: &Pose, end: &Pose, t: f32) -> Pose {
    Pose {
        left_arm: (
            start.left_arm.0 + (end.left_arm.0 - start.left_arm.0) * t,
            start.left_arm.1 + (end.left_arm.1 - start.left_arm.1) * t,
        ),
        right_arm: (
            start.right_arm.0 + (end.right_arm.0 - start.right_arm.0) * t,
            start.right_arm.1 + (end.right_arm.1 - start.right_arm.1) * t,
        ),
        left_leg: (
            start.left_leg.0 + (end.left_leg.0 - start.left_leg.0) * t,
            start.left_leg.1 + (end.left_leg.1 - start.left_leg.1) * t,
        ),
        right_leg: (
            start.right_leg.0 + (end.right_leg.0 - start.right_leg.0) * t,
            start.right_leg.1 + (end.right_leg.1 - start.right_leg.1) * t,
        ),
    }
}

struct Game {
    local_player: Player,
    other_players: Vec<Player>,
    last_send_time: Instant,
    send_interval: Duration,
    message_send_interval: Duration,
}

impl Game {
    fn new() -> Self {
        Self {
            local_player: Player::new_local(400.0, 300.0, 30.0, 30.0), // Start at center
            other_players: Vec::new(),
            last_send_time: Instant::now(),
            send_interval: Duration::from_millis(16), // ~60 updates per second
            message_send_interval: Duration::from_secs(1),
        }
    }

    fn update(&mut self, dt: f32) {
        self.handle_input(dt);
        self.local_player.update(dt);
        for player in &mut self.other_players {
            player.update(dt);
        }
    }

    fn handle_input(&mut self, dt: f32) {
        if is_key_pressed(KeyCode::R) {
            self.local_player.current_pose_index = 0;
            self.local_player.next_pose_index = 1;
            self.local_player.pose_interp_factor = 0.0;
            self.local_player.last_pose_update_time = Instant::now();
        }

        let mut direction = Vec2::ZERO;
        if is_key_down(KeyCode::W) {
            direction.y -= 1.0;
        }
        if is_key_down(KeyCode::S) {
            direction.y += 1.0;
        }
        if is_key_down(KeyCode::A) {
            direction.x -= 1.0;
        }
        if is_key_down(KeyCode::D) {
            direction.x += 1.0;
        }

        if is_key_pressed(KeyCode::Space) {
            let message = "Hello, world!".to_string();
            self.local_player.message = Some(message.clone());
            self.local_player.message_sent = false;
        }

        if is_key_pressed(KeyCode::G) {
            let message = "Come over here.".to_string();
            self.local_player.message = Some(message.clone());
            self.local_player.message_sent = false;
        }

        if is_key_pressed(KeyCode::H) {
            let message = "Okay.".to_string();
            self.local_player.message = Some(message.clone());
            self.local_player.message_sent = false;
        }

        // Determine if the player is moving via WASD
        let mut is_moving = false;
        if direction != Vec2::ZERO {
            direction = direction.normalize();
            self.local_player.x += direction.x * self.local_player.speed * dt;
            self.local_player.y += direction.y * self.local_player.speed * dt;
            self.local_player.position_changed = true;
            is_moving = true;

            // Clamp to screen
            self.local_player.x = self
                .local_player
                .x
                .clamp(0.0, 800.0 - self.local_player.width);
            self.local_player.y = self
                .local_player
                .y
                .clamp(0.0, 600.0 - self.local_player.height);
        }

        if is_mouse_button_pressed(MouseButton::Right) {
            // Changed from is_mouse_button_down
            let mouse_pos = mouse_position();
            self.local_player.target_x = Some(mouse_pos.0);
            self.local_player.target_y = Some(mouse_pos.1);
            is_moving = true;
        }

        // Determine if the player is moving based on input or target position
        self.local_player.is_moving = is_moving || self.local_player.target_x.is_some();
    }

    fn draw(&self) {
        let mut local_player_drawn = false;

        // Draw other players and insert the local player at the correct position
        for player in &self.other_players {
            if !local_player_drawn && self.local_player.y < player.y {
                self.local_player.draw();
                local_player_drawn = true;
            }
            player.draw();
        }

        // Draw the local player if it hasn't been drawn yet
        if !local_player_drawn {
            self.local_player.draw();
        }
    }
}

// Idle pose definition
const IDLE_POSE: Pose = Pose {
    left_arm: (-20.0, 30.0),
    right_arm: (20.0, 30.0),
    left_leg: (-10.0, 60.0),
    right_leg: (10.0, 60.0),
};

//window conf
fn window_conf() -> Conf {
    Conf {
        window_title: "Smooth Multiplayer Game".to_owned(),
        window_width: 800,
        window_height: 600,
        fullscreen: false,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    // Define the target frame rate and frame duration
    const TARGET_FPS: u32 = 60;
    const TARGET_FRAME_DURATION: Duration = Duration::from_millis(1000 / TARGET_FPS as u64);

    let rt = Runtime::new().unwrap();

    let (handler, listener) = node::split::<()>();
    let server_addr = "40.124.89.57:3042";

    let (server_endpoint, _) = handler
        .network()
        .connect(Transport::FramedTcp, server_addr)
        .expect("Failed to connect to server");

    let game = Arc::new(Mutex::new(Game::new()));
    let game_clone = Arc::clone(&game);
    let handler_clone = handler.clone();

    rt.spawn(async move {
        listener.for_each(move |event| {
            if let NodeEvent::Network(net_event) = event {
                match net_event {
                    NetEvent::Connected(_endpoint, _success) => {
                        println!("Connected to server");
                    }
                    NetEvent::Accepted(_, _) => unreachable!(),
                    NetEvent::Message(_endpoint, data) => {
                        match bincode::deserialize::<ClientMessage>(&data) {
                            Ok(message) => match message {
                                ClientMessage::PlayerPosition { id, x, y } => {
                                    let mut game = game_clone.lock().unwrap();
                                    if id != game.local_player.id {
                                        if let Some(player) =
                                            game.other_players.iter_mut().find(|p| p.id == id)
                                        {
                                            player.target_x = Some(x);
                                            player.target_y = Some(y);
                                        } else {
                                            game.other_players.push(Player::new_other(id, x, y));
                                        }
                                    }
                                }
                                ClientMessage::AssignPlayerId { id } => {
                                    let mut game = game_clone.lock().unwrap();
                                    println!("Assigned player id: {}", id);
                                    game.local_player.id = id;
                                }
                                ClientMessage::OtherPlayerDisconnected { id } => {
                                    let mut game = game_clone.lock().unwrap();
                                    game.other_players.retain(|p| p.id != id);
                                }
                                ClientMessage::UpdateMessage { id, message } => {
                                    let mut game = game_clone.lock().unwrap();
                                    if id != game.local_player.id {
                                        if let Some(player) =
                                            game.other_players.iter_mut().find(|p| p.id == id)
                                        {
                                            player.last_message_send_time = Instant::now();
                                            player.message = Some(message);
                                        }
                                    }
                                }
                            },
                            Err(e) => {
                                println!("Failed to deserialize message: {:?}", e);
                            }
                        }
                    }
                    NetEvent::Disconnected(_endpoint) => {
                        println!("Disconnected from server");
                    }
                }
            }
        });
    });

    loop {
        let frame_start = Instant::now();

        let dt = get_frame_time();

        // Update game state
        {
            let mut game = game.lock().unwrap();
            game.update(dt);
        }

        // Send heartbeat position to server every 1 second
        {
            let mut game = game.lock().unwrap();
            if game.local_player.id != 0 && game.last_send_time.elapsed() >= Duration::from_secs(1)
            {
                let message = ClientMessage::PlayerPosition {
                    id: game.local_player.id,
                    x: game.local_player.x,
                    y: game.local_player.y,
                };
                let serialized = bincode::serialize(&message).unwrap();
                handler_clone.network().send(server_endpoint, &serialized);
                println!("Sent heartbeat to server");
                game.last_send_time = Instant::now();
            }
        }

        // Send position update if enough time has passed
        {
            let mut game = game.lock().unwrap();
            if game.local_player.id != 0
                && game.last_send_time.elapsed() >= game.send_interval
                && game.local_player.position_changed
            {
                let message = ClientMessage::PlayerPosition {
                    id: game.local_player.id,
                    x: game.local_player.x,
                    y: game.local_player.y,
                };
                let serialized = bincode::serialize(&message).unwrap();
                handler_clone.network().send(server_endpoint, &serialized);
                game.last_send_time = Instant::now();
                game.local_player.position_changed = false;
            }
        }

        {
            let mut game = game.lock().unwrap();

            if game.local_player.last_message_send_time.elapsed() >= game.message_send_interval {
                if let Some(message) = &game.local_player.message {
                    if !game.local_player.message_sent {
                        let message = ClientMessage::UpdateMessage {
                            id: game.local_player.id,
                            message: message.clone(),
                        };
                        let serialized = bincode::serialize(&message).unwrap();
                        handler_clone.network().send(server_endpoint, &serialized);
                        println!("Sent message to server");
                        game.local_player.message_sent = true;
                        game.local_player.last_message_send_time = Instant::now();
                    }
                }
            }

            // After 15 seconds, clear the message
            if game.local_player.last_message_send_time.elapsed() >= Duration::from_secs(15) {
                game.local_player.message = None;
                game.local_player.last_message_send_time = Instant::now();
            }
        }

        // Render
        clear_background(WHITE);
        {
            let game = game.lock().unwrap();
            game.draw();
        }

        // Display FPS (optional)
        // draw_text(&format!("FPS: {}", get_fps()), 20.0, 20.0, 20.0, BLACK);

        // Advance to next frame
        next_frame().await;

        // Calculate frame duration
        let frame_duration = frame_start.elapsed();

        // Calculate remaining time to sleep
        if frame_duration < TARGET_FRAME_DURATION {
            let sleep_duration = TARGET_FRAME_DURATION - frame_duration;
            // Convert Duration to f32 seconds for macroquad's sleep
            let sleep_duration_secs = sleep_duration.as_secs_f32();
            sleep(Duration::from_secs_f32(sleep_duration_secs));
        } else {
            // Frame took longer than target; consider logging or handling this case
            // For example:
            // println!("Frame overrun: {:?}", frame_duration);
        }
    }
}
