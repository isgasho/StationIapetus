use crate::{
    actor::{Actor, TargetDescriptor},
    bot::{
        lower_body::{LowerBodyMachine, LowerBodyMachineInput},
        upper_body::{UpperBodyMachine, UpperBodyMachineInput},
    },
    character::Character,
    level::UpdateContext,
    message::Message,
    weapon::WeaponContainer,
    GameTime,
};
use rg3d::{
    animation::machine::{Machine, PoseNode},
    core::{
        algebra::{Matrix4, Point3, UnitQuaternion, Vector3},
        color::Color,
        math::{frustum::Frustum, ray::Ray, SmoothAngle, Vector3Ext},
        pool::Handle,
        rand::Rng,
        visitor::{Visit, VisitResult, Visitor},
    },
    engine::resource_manager::ResourceManager,
    physics::{
        dynamics::{BodyStatus, RigidBodyBuilder},
        geometry::{ColliderBuilder, InteractionGroups},
    },
    rand,
    scene::{
        self,
        base::BaseBuilder,
        graph::Graph,
        node::Node,
        physics::{Physics, RayCastOptions},
        transform::TransformBuilder,
        Scene, SceneDrawingContext,
    },
    utils::{
        log::{Log, MessageKind},
        navmesh::Navmesh,
    },
};
use std::{
    ops::{Deref, DerefMut},
    path::Path,
    sync::mpsc::Sender,
};

mod lower_body;
mod upper_body;

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BotKind {
    Mutant,
    Parasite,
    Zombie,
}

impl BotKind {
    pub fn from_id(id: i32) -> Result<Self, String> {
        match id {
            0 => Ok(BotKind::Mutant),
            1 => Ok(BotKind::Parasite),
            2 => Ok(BotKind::Zombie),
            _ => Err(format!("Invalid bot kind {}", id)),
        }
    }

    pub fn id(self) -> i32 {
        match self {
            BotKind::Mutant => 0,
            BotKind::Parasite => 1,
            BotKind::Zombie => 2,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            BotKind::Mutant => "Mutant",
            BotKind::Parasite => "Parasite",
            BotKind::Zombie => "Zombie",
        }
    }
}

#[derive(Debug)]
pub struct Target {
    position: Vector3<f32>,
    handle: Handle<Actor>,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            position: Default::default(),
            handle: Default::default(),
        }
    }
}

impl Visit for Target {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.position.visit("Position", visitor)?;
        self.handle.visit("Handle", visitor)?;

        visitor.leave_region()
    }
}

pub struct Bot {
    target: Option<Target>,
    kind: BotKind,
    model: Handle<Node>,
    character: Character,
    pub definition: &'static BotDefinition,
    lower_body_machine: LowerBodyMachine,
    upper_body_machine: UpperBodyMachine,
    last_health: f32,
    restoration_time: f32,
    path: Vec<Vector3<f32>>,
    move_target: Vector3<f32>,
    current_path_point: usize,
    frustum: Frustum,
    last_path_rebuild_time: f64,
    last_move_dir: Vector3<f32>,
    spine: Handle<Node>,
    yaw: SmoothAngle,
    pitch: SmoothAngle,
    attack_timeout: f32,
}

impl Deref for Bot {
    type Target = Character;

    fn deref(&self) -> &Self::Target {
        &self.character
    }
}

impl DerefMut for Bot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.character
    }
}

impl Default for Bot {
    fn default() -> Self {
        Self {
            character: Default::default(),
            kind: BotKind::Mutant,
            model: Default::default(),
            target: Default::default(),
            definition: Self::get_definition(BotKind::Mutant),
            lower_body_machine: Default::default(),
            upper_body_machine: Default::default(),
            last_health: 0.0,
            restoration_time: 0.0,
            path: Default::default(),
            move_target: Default::default(),
            current_path_point: 0,
            frustum: Default::default(),
            last_path_rebuild_time: -10.0,
            last_move_dir: Default::default(),
            spine: Default::default(),
            yaw: SmoothAngle {
                angle: 0.0,
                target: 0.0,
                speed: 260.0f32.to_radians(), // rad/s
            },
            pitch: SmoothAngle {
                angle: 0.0,
                target: 0.0,
                speed: 260.0f32.to_radians(), // rad/s
            },
            attack_timeout: 0.0,
        }
    }
}

pub struct BotDefinition {
    // Generic parameters.
    pub scale: f32,
    pub health: f32,
    pub kind: BotKind,
    pub walk_speed: f32,
    pub weapon_scale: f32,
    pub model: &'static str,
    pub weapon_hand_name: &'static str,
    pub left_leg_name: &'static str,
    pub right_leg_name: &'static str,
    pub spine: &'static str,
    pub v_aim_angle_hack: f32,
    pub can_use_weapons: bool,
    pub attack_damage: f32,
    pub attack_timestamp: f32,

    // Animations.
    pub idle_animation: &'static str,
    pub scream_animation: &'static str,
    pub attack_animation: &'static str,
    pub walk_animation: &'static str,
    pub aim_animation: &'static str,
    pub dying_animation: &'static str,
}

impl Bot {
    pub fn get_definition(kind: BotKind) -> &'static BotDefinition {
        match kind {
            BotKind::Mutant => {
                static DEFINITION: BotDefinition = BotDefinition {
                    kind: BotKind::Mutant,
                    model: "data/models/mutant.FBX",
                    attack_animation: "data/animations/mutant_attack_swipe.fbx",
                    scream_animation: "data/animations/mutant_scream.fbx",
                    idle_animation: "data/animations/mutant_idle.fbx",
                    walk_animation: "data/animations/mutant_walk.fbx",
                    aim_animation: "", // Empty because cannot use weapons.
                    dying_animation: "data/animations/mutant_dying.fbx",
                    weapon_hand_name: "Mutant:RightHand",
                    left_leg_name: "Mutant:LeftUpLeg",
                    right_leg_name: "Mutant:RightUpLeg",
                    spine: "", // Empty because cannot use weapons.
                    walk_speed: 0.7,
                    scale: 0.0065,
                    weapon_scale: 1.0,
                    health: 1000.0,
                    v_aim_angle_hack: 0.0,
                    can_use_weapons: false,
                    attack_damage: 120.0,
                    attack_timestamp: 1.1,
                };
                &DEFINITION
            }
            BotKind::Parasite => {
                static DEFINITION: BotDefinition = BotDefinition {
                    kind: BotKind::Parasite,
                    model: "data/models/parasite.FBX",
                    attack_animation: "data/animations/parasite_attack.fbx",
                    scream_animation: "data/animations/parasite_scream.fbx",
                    idle_animation: "data/animations/parasite_idle.fbx",
                    walk_animation: "data/animations/parasite_running.fbx",
                    aim_animation: "", // Empty because cannot use weapons.
                    dying_animation: "data/animations/parasite_dying.fbx",
                    weapon_hand_name: "RightHand",
                    left_leg_name: "LeftUpLeg",
                    right_leg_name: "RightUpLeg",
                    spine: "", // Empty because cannot use weapons.
                    walk_speed: 1.0,
                    scale: 0.0055,
                    weapon_scale: 1.0,
                    health: 300.0,
                    v_aim_angle_hack: 0.0,
                    can_use_weapons: false,
                    attack_damage: 40.0,
                    attack_timestamp: 1.1,
                };
                &DEFINITION
            }
            BotKind::Zombie => {
                static DEFINITION: BotDefinition = BotDefinition {
                    kind: BotKind::Parasite,
                    model: "data/models/zombie.fbx",
                    attack_animation: "data/animations/zombie_attack.fbx",
                    scream_animation: "data/animations/zombie_scream.fbx",
                    idle_animation: "data/animations/zombie_idle.fbx",
                    walk_animation: "data/animations/zombie_running.fbx",
                    aim_animation: "data/animations/zombie_aim_rifle.fbx",
                    dying_animation: "data/animations/zombie_dying.fbx",
                    weapon_hand_name: "mixamorig5:RightHand",
                    left_leg_name: "mixamorig5:LeftUpLeg",
                    right_leg_name: "mixamorig5:RightUpLeg",
                    spine: "Spine",
                    walk_speed: 1.2,
                    scale: 0.0055,
                    weapon_scale: 1.0,
                    health: 100.0,
                    v_aim_angle_hack: 12.0,
                    can_use_weapons: false,
                    attack_damage: 40.0,
                    attack_timestamp: 1.6,
                };
                &DEFINITION
            }
        }
    }

    pub async fn new(
        kind: BotKind,
        resource_manager: ResourceManager,
        scene: &mut Scene,
        position: Vector3<f32>,
        sender: Sender<Message>,
    ) -> Self {
        let definition = Self::get_definition(kind);

        let body_height = 0.60;
        let body_radius = 0.20;

        let model = resource_manager
            .request_model(Path::new(definition.model))
            .await
            .unwrap()
            .instantiate_geometry(scene);

        scene.graph[model]
            .local_transform_mut()
            .set_position(Vector3::new(0.0, -body_height * 0.5 - body_radius, 0.0))
            .set_scale(Vector3::new(
                definition.scale,
                definition.scale,
                definition.scale,
            ));

        let spine = scene.graph.find_by_name(model, definition.spine);
        if spine.is_none() {
            Log::writeln(
                MessageKind::Warning,
                "Spine bone not found, bot won't aim vertically!".to_owned(),
            );
        }

        let pivot = BaseBuilder::new()
            .with_children(&[model])
            .build(&mut scene.graph);

        let body = scene.physics.add_body(
            RigidBodyBuilder::new(BodyStatus::Dynamic)
                .lock_rotations()
                .translation(position.x, position.y, position.z)
                .build(),
        );
        scene.physics.add_collider(
            ColliderBuilder::capsule_y(body_height * 0.5, body_radius)
                .friction(0.0)
                .build(),
            body,
        );

        scene.physics_binder.bind(pivot, body.into());

        let hand = scene.graph.find_by_name(model, definition.weapon_hand_name);
        let wpn_scale = definition.weapon_scale * (1.0 / definition.scale);
        let weapon_pivot = BaseBuilder::new()
            .with_local_transform(
                TransformBuilder::new()
                    .with_local_scale(Vector3::new(wpn_scale, wpn_scale, wpn_scale))
                    .with_local_rotation(
                        UnitQuaternion::from_axis_angle(&Vector3::x_axis(), -90.0f32.to_radians())
                            * UnitQuaternion::from_axis_angle(
                                &Vector3::z_axis(),
                                -90.0f32.to_radians(),
                            ),
                    )
                    .build(),
            )
            .build(&mut scene.graph);

        scene.graph.link_nodes(weapon_pivot, hand);

        let locomotion_machine =
            LowerBodyMachine::new(resource_manager.clone(), &definition, model, scene).await;
        let combat_machine = UpperBodyMachine::new(
            resource_manager.clone(),
            definition,
            model,
            scene,
            definition.attack_timestamp,
        )
        .await;

        Self {
            character: Character {
                pivot,
                body,
                weapon_pivot,
                health: definition.health,
                sender: Some(sender),
                ..Default::default()
            },
            spine,
            definition,
            last_health: definition.health,
            model,
            kind,
            lower_body_machine: locomotion_machine,
            upper_body_machine: combat_machine,
            ..Default::default()
        }
    }

    pub fn can_be_removed(&self, scene: &Scene) -> bool {
        scene
            .animations
            .get(self.upper_body_machine.dying_animation)
            .has_ended()
    }

    pub fn can_shoot(&self) -> bool {
        self.upper_body_machine.machine.active_state() == self.upper_body_machine.aim_state
            && self.definition.can_use_weapons
    }

    fn select_target(
        &mut self,
        self_handle: Handle<Actor>,
        scene: &mut Scene,
        targets: &[TargetDescriptor],
    ) {
        // Check if existing target is valid.
        if let Some(target) = self.target.as_mut() {
            for target_desc in targets {
                if target_desc.handle != self_handle
                    && target_desc.handle == target.handle
                    && target_desc.health > 0.0
                {
                    target.position = target_desc.position;
                    return;
                }
            }
        }

        let position = self.character.position(&scene.physics);
        let mut closest_distance = std::f32::MAX;

        let mut query_buffer = Vec::default();
        'target_loop: for desc in targets {
            if desc.handle != self_handle && self.frustum.is_contains_point(desc.position) {
                let ray = Ray::from_two_points(&desc.position, &position).unwrap_or_default();
                scene.physics.cast_ray(
                    RayCastOptions {
                        ray,
                        groups: InteractionGroups::all(),
                        max_len: ray.dir.norm(),
                        sort_results: true,
                    },
                    &mut query_buffer,
                );

                'hit_loop: for hit in query_buffer.iter() {
                    let collider = scene.physics.colliders.get(hit.collider.into()).unwrap();
                    let body = collider.parent();

                    if collider.shape().as_trimesh().is_some() {
                        // Target is behind something.
                        continue 'target_loop;
                    } else {
                        // Prevent setting self as target.
                        if self.character.body == body.into() {
                            continue 'hit_loop;
                        }
                    }
                }

                let sqr_d = position.sqr_distance(&desc.position);
                if sqr_d < closest_distance {
                    self.target = Some(Target {
                        position: desc.position,
                        handle: desc.handle,
                    });
                    closest_distance = sqr_d;
                }
            }
        }
    }

    fn select_weapon(&mut self, weapons: &WeaponContainer) {
        if self.character.current_weapon().is_some()
            && weapons[self.character.current_weapon()].ammo() == 0
        {
            for (i, handle) in self.character.weapons().iter().enumerate() {
                if weapons[*handle].ammo() > 0 {
                    self.character.set_current_weapon(i);
                    break;
                }
            }
        }
    }

    pub fn debug_draw(&self, context: &mut SceneDrawingContext) {
        for pts in self.path.windows(2) {
            let a = pts[0];
            let b = pts[1];
            context.add_line(scene::Line {
                begin: a,
                end: b,
                color: Color::from_rgba(255, 0, 0, 255),
            });
        }

        context.draw_frustum(&self.frustum, Color::from_rgba(0, 200, 0, 255));
    }

    fn update_frustum(&mut self, position: Vector3<f32>, graph: &Graph) {
        let head_pos = position + Vector3::new(0.0, 0.4, 0.0);
        let up = graph[self.model].up_vector();
        let look_at = head_pos + graph[self.model].look_vector();
        let view_matrix = Matrix4::look_at_rh(&Point3::from(head_pos), &Point3::from(look_at), &up);
        let projection_matrix =
            Matrix4::new_perspective(16.0 / 9.0, 90.0f32.to_radians(), 0.1, 20.0);
        let view_projection_matrix = projection_matrix * view_matrix;
        self.frustum = Frustum::from(view_projection_matrix).unwrap();
    }

    fn aim_vertically(&mut self, look_dir: Vector3<f32>, graph: &mut Graph, time: GameTime) {
        let angle = self.pitch.angle();
        self.pitch
            .set_target(
                look_dir.dot(&Vector3::y()).acos() - std::f32::consts::PI / 2.0
                    + self.definition.v_aim_angle_hack.to_radians(),
            )
            .update(time.delta);

        if self.spine.is_some() {
            graph[self.spine]
                .local_transform_mut()
                .set_rotation(UnitQuaternion::from_axis_angle(&Vector3::x_axis(), angle));
        }
    }

    fn aim_horizontally(&mut self, look_dir: Vector3<f32>, physics: &mut Physics, time: GameTime) {
        let angle = self.yaw.angle();
        self.yaw
            .set_target(look_dir.x.atan2(look_dir.z))
            .update(time.delta);

        let body = physics.bodies.get_mut(self.body.into()).unwrap();
        let mut position = *body.position();
        position.rotation = UnitQuaternion::from_axis_angle(&Vector3::y_axis(), angle);
        body.set_position(position, true);
    }

    fn rebuild_path(&mut self, position: Vector3<f32>, navmesh: &mut Navmesh, time: GameTime) {
        if let Some(target) = self.target.as_ref() {
            let from = position - Vector3::new(0.0, 1.0, 0.0);
            if let Some(from_index) = navmesh.query_closest(from) {
                if let Some(to_index) = navmesh.query_closest(target.position) {
                    self.current_path_point = 0;
                    // Rebuild path if target path vertex has changed.
                    if navmesh
                        .build_path(from_index, to_index, &mut self.path)
                        .is_ok()
                    {
                        self.path.reverse();
                        self.last_path_rebuild_time = time.elapsed;
                    }
                }
            }
        }
    }

    pub fn set_target(&mut self, handle: Handle<Actor>, position: Vector3<f32>) {
        self.target = Some(Target { position, handle });
    }

    pub fn update(
        &mut self,
        self_handle: Handle<Actor>,
        context: &mut UpdateContext,
        targets: &[TargetDescriptor],
    ) {
        self.select_target(self_handle, context.scene, targets);
        self.select_weapon(context.weapons);

        let has_ground_contact = self.character.has_ground_contact(&context.scene.physics);
        let body = context
            .scene
            .physics
            .bodies
            .get_mut(self.character.body.into())
            .unwrap();
        let (in_close_combat, look_dir) = match self.target.as_ref() {
            None => (false, Vector3::z()),
            Some(target) => {
                let d = target.position - body.position().translation.vector;
                let close_combat_threshold = 0.75;
                (d.norm() <= close_combat_threshold, d)
            }
        };

        let position = body.position().translation.vector;

        if let Some(path_point) = self.path.get(self.current_path_point) {
            self.move_target = *path_point;
            if self.move_target.metric_distance(&position) <= 0.75
                && self.current_path_point < self.path.len() - 1
            {
                self.current_path_point += 1;
            }
        }

        self.update_frustum(position, &context.scene.graph);

        let was_damaged = self.character.health < self.last_health;
        if was_damaged {
            self.restoration_time = 0.8;
        }
        let can_aim = self.restoration_time <= 0.0;
        self.last_health = self.character.health;

        if self.is_dead() {
            for &animation in &[
                self.upper_body_machine.dying_animation,
                self.lower_body_machine.dying_animation,
            ] {
                context
                    .scene
                    .animations
                    .get_mut(animation)
                    .set_enabled(true);
            }

            for &animation in &[self.upper_body_machine.attack_animation] {
                context
                    .scene
                    .animations
                    .get_mut(animation)
                    .set_enabled(false);
            }
        }

        let mut is_moving = false;
        if !self.is_dead() && !in_close_combat && self.target.is_some() {
            if let Some(move_dir) = (self.move_target - position).try_normalize(std::f32::EPSILON) {
                let mut vel = move_dir.scale(self.definition.walk_speed);
                vel.y = body.linvel().y;
                body.set_linvel(vel, true);
                self.last_move_dir = move_dir;
                is_moving = true;
            }
        } else {
            body.set_linvel(Vector3::new(0.0, body.linvel().y, 0.0), true);
        }

        let sender = self.character.sender.as_ref().unwrap();

        if !in_close_combat && can_aim && self.can_shoot() && self.target.is_some() {
            if let Some(weapon) = self
                .character
                .weapons
                .get(self.character.current_weapon as usize)
            {
                sender
                    .send(Message::ShootWeapon {
                        weapon: *weapon,
                        direction: Some(look_dir),
                    })
                    .unwrap();
            }
        }

        // Apply damage to target from melee attack
        if let Some(target) = self.target.as_ref() {
            while let Some(event) = context
                .scene
                .animations
                .get_mut(self.upper_body_machine.attack_animation)
                .pop_event()
            {
                if event.signal_id == UpperBodyMachine::HIT_SIGNAL && in_close_combat {
                    sender
                        .send(Message::DamageActor {
                            actor: target.handle,
                            who: Default::default(),
                            amount: self.definition.attack_damage,
                        })
                        .unwrap();
                }
            }
        }

        // Emit step sounds from walking animation.
        if self.lower_body_machine.is_walking() {
            while let Some(event) = context
                .scene
                .animations
                .get_mut(self.lower_body_machine.walk_animation)
                .pop_event()
            {
                if event.signal_id == LowerBodyMachine::STEP_SIGNAL && has_ground_contact {
                    let footsteps = [
                        "data/sounds/footsteps/FootStep_shoe_stone_step1.wav",
                        "data/sounds/footsteps/FootStep_shoe_stone_step2.wav",
                        "data/sounds/footsteps/FootStep_shoe_stone_step3.wav",
                        "data/sounds/footsteps/FootStep_shoe_stone_step4.wav",
                    ];
                    sender
                        .send(Message::PlaySound {
                            path: footsteps[rand::thread_rng().gen_range(0..footsteps.len())]
                                .into(),
                            position,
                            gain: 1.0,
                            rolloff_factor: 2.0,
                            radius: 3.0,
                        })
                        .unwrap();
                }
            }
        }

        if context.time.elapsed - self.last_path_rebuild_time >= 1.0 {
            if context.navmesh.is_some() {
                let navmesh = &mut context.scene.navmeshes[context.navmesh];

                self.rebuild_path(position, navmesh, context.time);
            }
        }
        self.restoration_time -= context.time.delta;

        self.lower_body_machine.apply(
            context.scene,
            context.time,
            LowerBodyMachineInput {
                walk: is_moving,
                scream: false,
                dead: self.health <= 0.0,
            },
        );
        self.upper_body_machine.apply(
            context.scene,
            context.time,
            UpperBodyMachineInput {
                attack: in_close_combat && self.attack_timeout <= 0.0,
                walk: is_moving,
                scream: false,
                dead: self.health <= 0.0,
                aim: self.definition.can_use_weapons && can_aim,
            },
        );

        let attack_animation = context
            .scene
            .animations
            .get_mut(self.upper_body_machine.attack_animation);

        if in_close_combat {
            if self.attack_timeout <= 0.0
                && (attack_animation.has_ended() || !attack_animation.is_enabled())
            {
                attack_animation.set_enabled(true);
                attack_animation.rewind();
            }
        }

        if self.attack_timeout < 0.0 && attack_animation.has_ended() {
            self.attack_timeout = 0.3;
        }

        self.attack_timeout -= context.time.delta;

        // Aim overrides result of machines for spine bone.
        if !self.is_dead() {
            if let Some(look_dir) = look_dir.try_normalize(std::f32::EPSILON) {
                self.aim_vertically(look_dir, &mut context.scene.graph, context.time);
                self.aim_horizontally(look_dir, &mut context.scene.physics, context.time);
            }
        }
    }

    pub fn clean_up(&mut self, scene: &mut Scene) {
        self.upper_body_machine.clean_up(scene);
        self.lower_body_machine.clean_up(scene);
        self.character.clean_up(scene);
    }

    pub fn on_actor_removed(&mut self, handle: Handle<Actor>) {
        if let Some(target) = self.target.as_ref() {
            if target.handle == handle {
                self.target = None;
            }
        }
    }
}

fn clean_machine(machine: &Machine, scene: &mut Scene) {
    for node in machine.nodes() {
        if let PoseNode::PlayAnimation(node) = node {
            scene.animations.remove(node.animation);
        }
    }
}

impl Visit for Bot {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        let mut kind_id = self.kind.id();
        kind_id.visit("Kind", visitor)?;
        if visitor.is_reading() {
            self.kind = BotKind::from_id(kind_id)?;
        }

        self.definition = Self::get_definition(self.kind);
        self.character.visit("Character", visitor)?;
        self.model.visit("Model", visitor)?;
        self.target.visit("Target", visitor)?;
        self.lower_body_machine
            .visit("LocomotionMachine", visitor)?;
        self.upper_body_machine.visit("AimMachine", visitor)?;
        self.restoration_time.visit("RestorationTime", visitor)?;
        self.yaw.visit("Yaw", visitor)?;
        self.pitch.visit("Pitch", visitor)?;

        visitor.leave_region()
    }
}
