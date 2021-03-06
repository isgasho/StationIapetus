use crate::{
    character::Character,
    control_scheme::{ControlButton, ControlScheme},
    level::UpdateContext,
    message::Message,
    player::{
        lower_body::{LowerBodyMachine, LowerBodyMachineInput},
        upper_body::{CombatWeaponKind, UpperBodyMachine, UpperBodyMachineInput},
    },
    weapon::projectile::ProjectileKind,
};
use rg3d::{
    animation::{
        machine::{BlendPose, Machine, PoseNode, PoseWeight, State},
        Animation,
    },
    core::{
        algebra::{Isometry3, UnitQuaternion, Vector3},
        math::{self, ray::Ray, Matrix4Ext, SmoothAngle, Vector3Ext},
        pool::Handle,
        visitor::{Visit, VisitResult, Visitor},
    },
    engine::resource_manager::ResourceManager,
    event::{DeviceEvent, ElementState, Event, MouseScrollDelta, WindowEvent},
    physics::{dynamics::RigidBodyBuilder, geometry::ColliderBuilder},
    resource::{model::Model, texture::TextureWrapMode},
    scene::{
        base::BaseBuilder,
        camera::{CameraBuilder, SkyBox},
        graph::Graph,
        node::Node,
        physics::RayCastOptions,
        transform::TransformBuilder,
        ColliderHandle, Scene,
    },
};
use std::{
    ops::{Deref, DerefMut},
    sync::{mpsc::Sender, Arc, RwLock},
};

mod lower_body;
mod upper_body;

/// Creates a camera at given position with a skybox.
pub async fn create_camera(
    resource_manager: ResourceManager,
    position: Vector3<f32>,
    graph: &mut Graph,
) -> Handle<Node> {
    // Load skybox textures in parallel.
    let (front, back, left, right, top, bottom) = rg3d::futures::join!(
        resource_manager
            .request_texture("data/textures/skyboxes/DarkStormy/DarkStormyFront2048.png"),
        resource_manager
            .request_texture("data/textures/skyboxes/DarkStormy/DarkStormyBack2048.png"),
        resource_manager
            .request_texture("data/textures/skyboxes/DarkStormy/DarkStormyLeft2048.png"),
        resource_manager
            .request_texture("data/textures/skyboxes/DarkStormy/DarkStormyRight2048.png"),
        resource_manager.request_texture("data/textures/skyboxes/DarkStormy/DarkStormyUp2048.png"),
        resource_manager
            .request_texture("data/textures/skyboxes/DarkStormy/DarkStormyDown2048.png")
    );

    // Unwrap everything.
    let skybox = SkyBox {
        front: Some(front.unwrap()),
        back: Some(back.unwrap()),
        left: Some(left.unwrap()),
        right: Some(right.unwrap()),
        top: Some(top.unwrap()),
        bottom: Some(bottom.unwrap()),
    };

    // Set S and T coordinate wrap mode, ClampToEdge will remove any possible seams on edges
    // of the skybox.
    for skybox_texture in skybox.textures().iter().filter_map(|t| t.clone()) {
        let mut data = skybox_texture.data_ref();
        data.set_s_wrap_mode(TextureWrapMode::ClampToEdge);
        data.set_t_wrap_mode(TextureWrapMode::ClampToEdge);
    }

    // Camera is our eyes in the world - you won't see anything without it.
    CameraBuilder::new(
        BaseBuilder::new().with_local_transform(
            TransformBuilder::new()
                .with_local_position(position)
                .build(),
        ),
    )
    .with_skybox(skybox)
    .build(graph)
}

pub struct WalkStateDefinition {
    state: Handle<State>,
    walk_animation: Handle<Animation>,
    run_animation: Handle<Animation>,
}

pub fn make_walk_state(
    machine: &mut Machine,
    scene: &mut Scene,
    model: Handle<Node>,
    walk_animation_resource: Model,
    run_animation_resource: Model,
    walk_factor: String,
    run_factor: String,
) -> WalkStateDefinition {
    let walk_animation = *walk_animation_resource
        .retarget_animations(model, scene)
        .get(0)
        .unwrap();
    let walk_animation_node = machine.add_node(PoseNode::make_play_animation(walk_animation));

    let run_animation = *run_animation_resource
        .retarget_animations(model, scene)
        .get(0)
        .unwrap();
    let run_animation_node = machine.add_node(PoseNode::make_play_animation(run_animation));

    let walk_node = machine.add_node(PoseNode::make_blend_animations(vec![
        BlendPose::new(PoseWeight::Parameter(walk_factor), walk_animation_node),
        BlendPose::new(PoseWeight::Parameter(run_factor), run_animation_node),
    ]));

    WalkStateDefinition {
        state: machine.add_state(State::new("Walk", walk_node)),
        walk_animation,
        run_animation,
    }
}

#[derive(Default)]
pub struct InputController {
    walk_forward: bool,
    walk_backward: bool,
    walk_left: bool,
    walk_right: bool,
    jump: bool,
    yaw: f32,
    pitch: f32,
    aim: bool,
    toss_grenade: bool,
    shoot: bool,
    run: bool,
}

impl Deref for Player {
    type Target = Character;

    fn deref(&self) -> &Self::Target {
        &self.character
    }
}

impl DerefMut for Player {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.character
    }
}

#[derive(Copy, Clone, PartialOrd, PartialEq, Eq, Ord)]
#[repr(u32)]
enum Direction {
    None,
    Next,
    Previous,
}

impl Direction {
    fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 => Ok(Self::None),
            1 => Ok(Self::Next),
            2 => Ok(Self::Previous),
            _ => Err(format!("Invalid Direction id {}!", id)),
        }
    }
}

impl Default for Direction {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Default)]
pub struct Player {
    character: Character,
    camera_pivot: Handle<Node>,
    camera_hinge: Handle<Node>,
    camera: Handle<Node>,
    model: Handle<Node>,
    controller: InputController,
    lower_body_machine: LowerBodyMachine,
    upper_body_machine: UpperBodyMachine,
    model_yaw: SmoothAngle,
    spine_pitch: SmoothAngle,
    spine: Handle<Node>,
    hips: Handle<Node>,
    move_speed: f32,
    camera_offset: Vector3<f32>,
    target_camera_offset: Vector3<f32>,
    collider: ColliderHandle,
    control_scheme: Option<Arc<RwLock<ControlScheme>>>,
    weapon_change_direction: Direction,
    weapon_yaw_correction: SmoothAngle,
    weapon_pitch_correction: SmoothAngle,
    weapon_origin: Handle<Node>,
    run_factor: f32,
    target_run_factor: f32,
    in_air_time: f32,
    velocity: Vector3<f32>, // Horizontal velocity, Y is ignored.
    target_velocity: Vector3<f32>,
}

impl Visit for Player {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.character.visit("Character", visitor)?;
        self.camera_pivot.visit("CameraPivot", visitor)?;
        self.camera_hinge.visit("CameraHinge", visitor)?;
        self.camera.visit("Camera", visitor)?;
        self.model.visit("Model", visitor)?;
        self.lower_body_machine.visit("LowerBodyMachine", visitor)?;
        self.upper_body_machine.visit("UpperBodyMachine", visitor)?;
        self.model_yaw.visit("ModelYaw", visitor)?;
        self.spine_pitch.visit("SpinePitch", visitor)?;
        self.hips.visit("Hips", visitor)?;
        self.spine.visit("Spine", visitor)?;
        self.move_speed.visit("MoveSpeed", visitor)?;
        self.camera_offset.visit("CameraOffset", visitor)?;
        self.target_camera_offset
            .visit("TargetCameraOffset", visitor)?;
        self.collider.visit("Collider", visitor)?;
        self.weapon_origin.visit("WeaponOrigin", visitor)?;
        self.weapon_yaw_correction
            .visit("WeaponYawCorrection", visitor)?;
        self.weapon_pitch_correction
            .visit("WeaponPitchCorrection", visitor)?;
        self.run_factor.visit("RunFactor", visitor)?;
        self.target_run_factor.visit("TargetRunFactor", visitor)?;
        self.in_air_time.visit("InAirTime", visitor)?;
        self.velocity.visit("Velocity", visitor)?;
        self.target_velocity.visit("TargetVelocity", visitor)?;

        let mut direction = self.weapon_change_direction as u32;
        direction.visit("WeaponChangeDirection", visitor)?;
        if visitor.is_reading() {
            self.weapon_change_direction = Direction::from_id(direction)?;
        }

        visitor.leave_region()
    }
}

impl Player {
    pub async fn new(
        scene: &mut Scene,
        resource_manager: ResourceManager,
        position: Vector3<f32>,
        sender: Sender<Message>,
        control_scheme: Arc<RwLock<ControlScheme>>,
    ) -> Self {
        let body_radius = 0.2;
        let body_height = 0.25;
        let camera_offset = -0.8;

        let camera;
        let camera_hinge;
        let camera_pivot = BaseBuilder::new()
            .with_children(&[{
                camera_hinge = BaseBuilder::new()
                    .with_local_transform(
                        TransformBuilder::new()
                            .with_local_position(Vector3::new(-0.22, 0.25, 0.0))
                            .build(),
                    )
                    .with_children(&[{
                        camera = create_camera(
                            resource_manager.clone(),
                            Vector3::new(0.0, 0.0, camera_offset),
                            &mut scene.graph,
                        )
                        .await;
                        camera
                    }])
                    .build(&mut scene.graph);
                camera_hinge
            }])
            .build(&mut scene.graph);

        let model_resource = resource_manager
            .request_model("data/models/agent.fbx")
            .await
            .unwrap();

        let model_handle = model_resource.instantiate_geometry(scene);

        scene.graph[model_handle]
            .local_transform_mut()
            .set_position(Vector3::new(0.0, -body_height - body_radius, 0.0))
            // Our model is too big, fix it by scale.
            .set_scale(Vector3::new(0.005, 0.005, 0.005));

        let pivot = BaseBuilder::new()
            .with_children(&[model_handle])
            .build(&mut scene.graph);

        let capsule = ColliderBuilder::capsule_y(body_height, body_radius)
            .friction(0.0)
            .build();
        let body = scene.physics.add_body(
            RigidBodyBuilder::new_dynamic()
                .lock_rotations()
                .position(Isometry3::new(position, Default::default()))
                .build(),
        );
        let collider = scene.physics.add_collider(capsule, body);

        scene.physics_binder.bind(pivot, body.into());

        let locomotion_machine =
            LowerBodyMachine::new(scene, model_handle, resource_manager.clone()).await;

        let combat_machine =
            UpperBodyMachine::new(scene, model_handle, resource_manager.clone()).await;

        scene.graph.update_hierarchical_data();

        let hand = scene
            .graph
            .find_by_name(model_handle, "mixamorig:RightHand");

        let hand_scale = scene.graph.global_scale(hand);

        let weapon_pivot;
        let weapon_origin = BaseBuilder::new()
            .with_local_transform(
                TransformBuilder::new()
                    .with_local_scale(Vector3::new(
                        1.0 / hand_scale.x,
                        1.0 / hand_scale.y,
                        1.0 / hand_scale.z,
                    ))
                    .with_local_rotation(
                        UnitQuaternion::from_axis_angle(&Vector3::x_axis(), -90.0f32.to_radians())
                            * UnitQuaternion::from_axis_angle(
                                &Vector3::z_axis(),
                                -90.0f32.to_radians(),
                            ),
                    )
                    .build(),
            )
            .with_children(&[{
                weapon_pivot = BaseBuilder::new().build(&mut scene.graph);
                weapon_pivot
            }])
            .build(&mut scene.graph);

        scene.graph.link_nodes(weapon_origin, hand);

        Self {
            character: Character {
                pivot,
                body: body.into(),
                weapon_pivot,
                sender: Some(sender),
                ..Default::default()
            },
            weapon_origin,
            model: model_handle,
            camera_pivot,
            controller: Default::default(),
            lower_body_machine: locomotion_machine,
            camera_hinge,
            camera,
            upper_body_machine: combat_machine,
            spine: scene.graph.find_by_name(model_handle, "mixamorig:Spine"),
            hips: scene.graph.find_by_name(model_handle, "mixamorig:Hips"),
            model_yaw: SmoothAngle {
                angle: 0.0,
                target: 0.0,
                speed: 10.0,
            },
            move_speed: 0.65,
            spine_pitch: SmoothAngle {
                angle: 0.0,
                target: 0.0,
                speed: 10.0,
            },
            camera_offset: Vector3::new(0.0, 0.0, camera_offset),
            target_camera_offset: Vector3::new(0.0, 0.0, camera_offset),
            collider,
            control_scheme: Some(control_scheme),
            weapon_change_direction: Direction::None,
            weapon_yaw_correction: SmoothAngle {
                angle: 0.0,
                target: 30.0f32.to_radians(),
                speed: 10.00,
            },
            weapon_pitch_correction: SmoothAngle {
                angle: 0.0,
                target: 10.0f32.to_radians(),
                speed: 10.00,
            },
            in_air_time: 0.0,
            velocity: Default::default(),
            run_factor: 0.0,
            target_run_factor: 0.0,
            target_velocity: Default::default(),
        }
    }

    pub fn set_control_scheme(&mut self, control_scheme: Arc<RwLock<ControlScheme>>) {
        self.control_scheme = Some(control_scheme);
    }

    pub fn camera(&self) -> Handle<Node> {
        self.camera
    }

    pub fn can_be_removed(&self, _scene: &Scene) -> bool {
        self.health <= 0.0
    }

    pub fn update(&mut self, context: &mut UpdateContext) {
        let UpdateContext { time, scene, .. } = context;

        let mut sound_context = scene.sound_context.state();
        let listener = sound_context.listener_mut();
        let camera = &scene.graph[self.camera];
        listener.set_basis(camera.global_transform().basis());
        listener.set_position(camera.global_position());
        std::mem::drop(sound_context);

        let mut has_ground_contact = false;
        if let Some(iterator) = scene
            .physics
            .narrow_phase
            .contacts_with(self.collider.into())
        {
            'outer_loop: for (_, _, contact) in iterator {
                for manifold in contact.manifolds.iter() {
                    if manifold.local_n1.y > 0.7 {
                        has_ground_contact = true;
                        break 'outer_loop;
                    }
                }
            }
        }

        let is_walking = self.controller.walk_backward
            || self.controller.walk_forward
            || self.controller.walk_right
            || self.controller.walk_left;
        let is_jumping = has_ground_contact && self.controller.jump;

        self.lower_body_machine.apply(
            scene,
            time.delta,
            LowerBodyMachineInput {
                is_walking,
                is_jumping,
                has_ground_contact: self.in_air_time <= 0.3,
                run_factor: self.run_factor,
            },
        );

        self.upper_body_machine.apply(
            scene,
            time.delta,
            UpperBodyMachineInput {
                is_walking,
                is_jumping,
                has_ground_contact: self.in_air_time <= 0.3,
                is_aiming: self.controller.aim,
                toss_grenade: self.controller.toss_grenade,
                weapon: CombatWeaponKind::Rifle,
                change_weapon: self.weapon_change_direction != Direction::None,
                run_factor: self.run_factor,
            },
        );
        if self.controller.run {
            self.target_run_factor = 1.0;
        } else {
            self.target_run_factor = 0.0;
        }
        self.run_factor += (self.target_run_factor - self.run_factor) * 0.1;

        let body = scene.physics.bodies.get_mut(self.body.into()).unwrap();

        let pivot = &scene.graph[self.pivot];

        let look_vector = pivot
            .look_vector()
            .try_normalize(std::f32::EPSILON)
            .unwrap_or(Vector3::z());

        let side_vector = pivot
            .side_vector()
            .try_normalize(std::f32::EPSILON)
            .unwrap_or(Vector3::x());

        let position = pivot.local_transform().position();

        self.target_velocity = Vector3::default();

        if self.controller.walk_right {
            self.target_velocity -= side_vector;
        }
        if self.controller.walk_left {
            self.target_velocity += side_vector;
        }
        if self.controller.walk_forward {
            self.target_velocity += look_vector;
        }
        if self.controller.walk_backward {
            self.target_velocity -= look_vector;
        }

        let can_move = self.lower_body_machine.machine.active_state()
            != self.lower_body_machine.fall_state
            && self.lower_body_machine.machine.active_state() != self.lower_body_machine.land_state;

        let speed = if can_move {
            math::lerpf(self.move_speed, self.move_speed * 4.0, self.run_factor) * time.delta
        } else {
            0.0
        };

        self.target_velocity = self
            .target_velocity
            .try_normalize(std::f32::EPSILON)
            .and_then(|v| Some(v.scale(speed)))
            .unwrap_or(Vector3::default());

        self.velocity.follow(&self.target_velocity, 0.15);

        let mut new_y_vel = None;
        while let Some(event) = scene
            .animations
            .get_mut(self.lower_body_machine.jump_animation)
            .pop_event()
        {
            if event.signal_id == LowerBodyMachine::JUMP_SIGNAL
                && (self.lower_body_machine.machine.active_transition()
                    == self.lower_body_machine.idle_to_jump
                    || self.lower_body_machine.machine.active_transition()
                        == self.lower_body_machine.walk_to_jump
                    || self.lower_body_machine.machine.active_state()
                        == self.lower_body_machine.jump_state)
            {
                new_y_vel = Some(3.0 * time.delta);
            }
        }

        while let Some(event) = scene
            .animations
            .get_mut(self.upper_body_machine.grab_animation)
            .pop_event()
        {
            if event.signal_id == UpperBodyMachine::GRAB_WEAPON_SIGNAL {
                match self.weapon_change_direction {
                    Direction::None => (),
                    Direction::Next => self.next_weapon(),
                    Direction::Previous => self.prev_weapon(),
                }

                self.weapon_change_direction = Direction::None;
            }
        }

        while let Some(event) = scene
            .animations
            .get_mut(self.upper_body_machine.put_back_animation)
            .pop_event()
        {
            if event.signal_id == UpperBodyMachine::PUT_BACK_WEAPON_END_SIGNAL {
                scene
                    .animations
                    .get_mut(self.upper_body_machine.grab_animation)
                    .set_enabled(true);
            }
        }

        while let Some(event) = scene
            .animations
            .get_mut(self.upper_body_machine.toss_grenade_animation)
            .pop_event()
        {
            if event.signal_id == UpperBodyMachine::TOSS_GRENADE_SIGNAL {
                let position = scene.graph[self.weapon_pivot].global_position();
                let direction = scene.graph[self.camera].look_vector();

                self.sender
                    .as_ref()
                    .unwrap()
                    .send(Message::CreateProjectile {
                        kind: ProjectileKind::Grenade,
                        position,
                        direction,
                        initial_velocity: direction.scale(15.0),
                        owner: Default::default(),
                    })
                    .unwrap();
            }
        }

        let quat_yaw = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), self.controller.yaw);

        body.wake_up(true);
        body.set_angvel(Default::default(), true);
        if let Some(new_y_vel) = new_y_vel {
            body.set_linvel(
                Vector3::new(
                    self.velocity.x / time.delta,
                    new_y_vel / time.delta,
                    self.velocity.z / time.delta,
                ),
                true,
            );
        } else {
            body.set_linvel(
                Vector3::new(
                    self.velocity.x / time.delta,
                    body.linvel().y,
                    self.velocity.z / time.delta,
                ),
                true,
            );
        }

        if self.controller.aim {
            self.spine_pitch.set_target(self.controller.pitch);
        } else {
            self.spine_pitch.set_target(0.0);
        }

        self.spine_pitch.update(time.delta);

        if is_walking || self.controller.aim {
            // Since we have free camera while not moving, we have to sync rotation of pivot
            // with rotation of camera so character will start moving in look direction.
            let mut current_position = *body.position();
            current_position.rotation = quat_yaw;
            body.set_position(current_position, true);

            // Apply additional rotation to model - it will turn in front of walking direction.
            let angle: f32 = if self.controller.aim {
                if self.controller.walk_left {
                    if self.controller.walk_backward {
                        -45.0
                    } else {
                        45.0
                    }
                } else if self.controller.walk_right {
                    if self.controller.walk_backward {
                        45.0
                    } else {
                        -45.0
                    }
                } else {
                    0.0
                }
            } else {
                if self.controller.walk_left {
                    if self.controller.walk_forward {
                        45.0
                    } else if self.controller.walk_backward {
                        135.0
                    } else {
                        90.0
                    }
                } else if self.controller.walk_right {
                    if self.controller.walk_forward {
                        -45.0
                    } else if self.controller.walk_backward {
                        -135.0
                    } else {
                        -90.0
                    }
                } else {
                    if self.controller.walk_backward {
                        180.0
                    } else {
                        0.0
                    }
                }
            };

            self.model_yaw
                .set_target(angle.to_radians())
                .update(time.delta);

            let mut additional_hips_rotation = Default::default();
            if self.controller.aim {
                scene.graph[self.model]
                    .local_transform_mut()
                    .set_rotation(UnitQuaternion::from_axis_angle(&Vector3::y_axis(), 0.0));

                let spine_transform = scene.graph[self.spine].local_transform_mut();
                spine_transform.set_rotation(
                    spine_transform.rotation()
                        * UnitQuaternion::from_axis_angle(
                            &Vector3::x_axis(),
                            self.spine_pitch.angle,
                        )
                        * UnitQuaternion::from_axis_angle(
                            &Vector3::y_axis(),
                            -(self.model_yaw.angle + 37.5f32.to_radians()),
                        ),
                );
                additional_hips_rotation =
                    UnitQuaternion::from_axis_angle(&Vector3::y_axis(), self.model_yaw.angle);
            } else {
                scene.graph[self.model].local_transform_mut().set_rotation(
                    UnitQuaternion::from_axis_angle(&Vector3::y_axis(), self.model_yaw.angle),
                );

                scene.graph[self.spine].local_transform_mut().set_rotation(
                    UnitQuaternion::from_axis_angle(&Vector3::x_axis(), self.spine_pitch.angle)
                        * UnitQuaternion::from_axis_angle(&Vector3::y_axis(), 0.0),
                );
            }

            scene.graph[self.hips].local_transform_mut().set_rotation(
                additional_hips_rotation
                    * UnitQuaternion::from_axis_angle(
                        &Vector3::x_axis(),
                        math::lerpf(5.0f32.to_radians(), 17.0f32.to_radians(), self.run_factor),
                    ),
            );

            let walk_dir = if self.controller.aim && self.controller.walk_backward {
                -1.0
            } else {
                1.0
            };

            for &animation in &[
                self.lower_body_machine.walk_animation,
                self.upper_body_machine.walk_animation,
                self.lower_body_machine.run_animation,
                self.upper_body_machine.run_animation,
            ] {
                scene.animations.get_mut(animation).set_speed(walk_dir);
            }
        }

        if self.controller.aim {
            self.weapon_yaw_correction.set_target(-4.0f32.to_radians());
            self.weapon_pitch_correction
                .set_target(-12.0f32.to_radians());
        } else {
            self.weapon_yaw_correction.set_target(30.0f32.to_radians());
            self.weapon_pitch_correction.set_target(8.0f32.to_radians());
        }

        let yaw_correction_angle = self.weapon_yaw_correction.update(time.delta).angle();
        let pitch_correction_angle = self.weapon_pitch_correction.update(time.delta).angle();
        scene.graph[self.weapon_pivot]
            .local_transform_mut()
            .set_rotation(
                UnitQuaternion::from_axis_angle(&Vector3::y_axis(), yaw_correction_angle)
                    * UnitQuaternion::from_axis_angle(&Vector3::x_axis(), pitch_correction_angle),
            );

        let ray_origin = scene.graph[self.camera_hinge].global_position();
        let ray_end = scene.graph[self.camera].global_position();
        let dir = (ray_end - ray_origin)
            .try_normalize(std::f32::EPSILON)
            .unwrap_or_default()
            .scale(10.0);
        let ray = Ray {
            origin: ray_origin,
            dir,
        };
        let mut results = Vec::new();
        scene.physics.cast_ray(
            RayCastOptions {
                ray,
                max_len: ray.dir.norm(),
                groups: Default::default(),
                sort_results: true,
            },
            &mut results,
        );

        if is_walking {
            let (kx, ky) = if self.controller.run {
                (8.0, 13.0)
            } else {
                (5.0, 10.0)
            };

            self.target_camera_offset.x = 0.015 * (time.elapsed as f32 * kx).cos();
            self.target_camera_offset.y = 0.015 * (time.elapsed as f32 * ky).sin();
        } else {
            self.target_camera_offset.x = 0.0;
            self.target_camera_offset.y = 0.0;
        }

        self.target_camera_offset.z = if self.controller.aim { 0.2 } else { 0.8 };

        for result in results {
            if result.collider != self.collider {
                let new_offset = (result.toi.min(0.8) - 0.2).max(0.1);
                if new_offset < self.target_camera_offset.z {
                    self.target_camera_offset.z = new_offset;
                }
                break;
            }
        }

        self.camera_offset.follow(&self.target_camera_offset, 0.2);

        scene.graph[self.camera]
            .local_transform_mut()
            .set_position(Vector3::new(
                self.camera_offset.x,
                self.camera_offset.y,
                -self.camera_offset.z,
            ));

        scene.graph[self.camera_pivot]
            .local_transform_mut()
            .set_rotation(quat_yaw)
            .set_position(position + self.velocity);

        // Rotate camera hinge - this will make camera move up and down while look at character
        // (well not exactly on character - on characters head)
        scene.graph[self.camera_hinge]
            .local_transform_mut()
            .set_rotation(UnitQuaternion::from_axis_angle(
                &Vector3::x_axis(),
                self.controller.pitch,
            ));

        if has_ground_contact {
            self.in_air_time = 0.0;
        } else {
            self.in_air_time += time.delta;
        }

        if has_ground_contact && self.controller.jump {
            // Rewind jump animation to beginning before jump.
            scene
                .animations
                .get_mut(self.lower_body_machine.jump_animation)
                .rewind();
            scene
                .animations
                .get_mut(self.upper_body_machine.jump_animation)
                .rewind();
        }

        if !has_ground_contact {
            scene
                .animations
                .get_mut(self.lower_body_machine.land_animation)
                .rewind();
            scene
                .animations
                .get_mut(self.upper_body_machine.land_animation)
                .rewind();
        }

        if let Some(current_weapon_handle) = self
            .character
            .weapons
            .get(self.character.current_weapon as usize)
        {
            if self.controller.shoot
                && self.upper_body_machine.machine.active_state()
                    == self.upper_body_machine.aim_state
            {
                self.character
                    .sender
                    .as_ref()
                    .unwrap()
                    .send(Message::ShootWeapon {
                        weapon: *current_weapon_handle,
                        direction: Some(scene.graph[self.camera].look_vector()),
                    })
                    .unwrap();
            }
        }
    }

    pub fn process_input_event(&mut self, event: &Event<()>, dt: f32, scene: &mut Scene) {
        let scheme = self.control_scheme.clone().unwrap();
        let scheme = scheme.read().unwrap();

        let button_state = match event {
            Event::WindowEvent { event, .. } => {
                if let WindowEvent::KeyboardInput { input, .. } = event {
                    input
                        .virtual_keycode
                        .map(|vk| (ControlButton::Key(vk), input.state))
                } else {
                    None
                }
            }
            Event::DeviceEvent { event, .. } => match event {
                &DeviceEvent::MouseWheel { delta } => match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        if y < 0.0 {
                            Some((ControlButton::WheelDown, ElementState::Pressed))
                        } else {
                            Some((ControlButton::WheelUp, ElementState::Pressed))
                        }
                    }
                    MouseScrollDelta::PixelDelta(delta) => {
                        if delta.y < 0.0 {
                            Some((ControlButton::WheelDown, ElementState::Pressed))
                        } else {
                            Some((ControlButton::WheelUp, ElementState::Pressed))
                        }
                    }
                },
                &DeviceEvent::Button { button, state } => {
                    Some((ControlButton::Mouse(button as u16), state))
                }
                DeviceEvent::MouseMotion { delta } => {
                    let mouse_sens = scheme.mouse_sens * dt;
                    self.controller.yaw -= (delta.0 as f32) * mouse_sens;
                    self.controller.pitch = (self.controller.pitch + (delta.1 as f32) * mouse_sens)
                        .max(-90.0f32.to_radians())
                        .min(90.0f32.to_radians());
                    None
                }
                _ => None,
            },
            _ => None,
        };

        if let Some((button, state)) = button_state {
            if button == scheme.aim.button {
                self.controller.aim = state == ElementState::Pressed;
            } else if button == scheme.move_forward.button {
                self.controller.walk_forward = state == ElementState::Pressed;
            } else if button == scheme.move_backward.button {
                self.controller.walk_backward = state == ElementState::Pressed;
            } else if button == scheme.move_left.button {
                self.controller.walk_left = state == ElementState::Pressed;
            } else if button == scheme.move_right.button {
                self.controller.walk_right = state == ElementState::Pressed;
            } else if button == scheme.jump.button {
                self.controller.jump = state == ElementState::Pressed;
            } else if button == scheme.run.button {
                self.controller.run = state == ElementState::Pressed;
            } else if button == scheme.next_weapon.button {
                if state == ElementState::Pressed
                    && self.current_weapon < self.weapons.len() as u32 - 1
                {
                    self.weapon_change_direction = Direction::Next;

                    scene
                        .animations
                        .get_mut(self.upper_body_machine.put_back_animation)
                        .rewind();

                    scene
                        .animations
                        .get_mut(self.upper_body_machine.grab_animation)
                        .set_enabled(false)
                        .rewind();
                }
            } else if button == scheme.prev_weapon.button {
                if state == ElementState::Pressed && self.current_weapon > 0 {
                    self.weapon_change_direction = Direction::Previous;

                    scene
                        .animations
                        .get_mut(self.upper_body_machine.put_back_animation)
                        .rewind();

                    scene
                        .animations
                        .get_mut(self.upper_body_machine.grab_animation)
                        .set_enabled(false)
                        .rewind();
                }
            } else if button == scheme.toss_grenade.button {
                self.controller.toss_grenade = state == ElementState::Pressed;
                if state == ElementState::Pressed {
                    scene
                        .animations
                        .get_mut(self.upper_body_machine.toss_grenade_animation)
                        .rewind();
                }
            } else if button == scheme.shoot.button {
                self.controller.shoot = state == ElementState::Pressed;
            }
        }
    }
}
