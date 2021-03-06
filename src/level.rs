use crate::{
    actor::{Actor, ActorContainer},
    bot::{Bot, BotKind},
    control_scheme::ControlScheme,
    effects::{self, EffectKind},
    item::{Item, ItemContainer, ItemKind},
    message::Message,
    player::Player,
    weapon::{
        projectile::{Projectile, ProjectileContainer, ProjectileKind},
        ray_hit, Weapon, WeaponContainer, WeaponKind,
    },
    GameEngine, GameTime,
};
use rg3d::{
    core::{
        algebra::{UnitQuaternion, Vector3},
        color::Color,
        math::{aabb::AxisAlignedBoundingBox, ray::Ray, PositionProvider},
        pool::Handle,
        visitor::{Visit, VisitResult, Visitor},
    },
    engine::resource_manager::ResourceManager,
    event::Event,
    physics::{
        crossbeam,
        geometry::{ContactEvent, InteractionGroups, ProximityEvent},
        pipeline::ChannelEventCollector,
    },
    renderer::surface::{SurfaceBuilder, SurfaceSharedData},
    scene::{
        self,
        base::BaseBuilder,
        mesh::{MeshBuilder, RenderPath},
        node::Node,
        physics::RayCastOptions,
        transform::TransformBuilder,
        Scene,
    },
    sound::{
        context::{self, Context},
        effects::{BaseEffect, Effect, EffectInput},
        source::{generic::GenericSourceBuilder, spatial::SpatialSourceBuilder, Status},
    },
    utils::navmesh::Navmesh,
};
use std::{
    path::{Path, PathBuf},
    sync::{mpsc::Sender, Arc, RwLock},
    time::Duration,
};

pub const RESPAWN_TIME: f32 = 4.0;

#[derive(Default)]
pub struct SoundManager {
    context: Context,
    reverb: Handle<Effect>,
}

impl SoundManager {
    pub fn new(context: Context) -> Self {
        let mut base_effect = BaseEffect::default();
        base_effect.set_gain(0.7);
        let mut reverb = rg3d::sound::effects::reverb::Reverb::new(base_effect);
        reverb.set_dry(0.5);
        reverb.set_wet(0.5);
        reverb.set_decay_time(Duration::from_secs_f32(3.0));
        let reverb = context
            .state()
            .add_effect(rg3d::sound::effects::Effect::Reverb(reverb));

        let hrtf_sphere = rg3d::sound::hrtf::HrirSphere::from_file(
            "data/sounds/IRC_1040_C.bin",
            context::SAMPLE_RATE,
        )
        .unwrap();
        context
            .state()
            .set_renderer(rg3d::sound::renderer::Renderer::HrtfRenderer(
                rg3d::sound::renderer::hrtf::HrtfRenderer::new(hrtf_sphere),
            ));

        Self { context, reverb }
    }

    pub async fn handle_message(&mut self, resource_manager: ResourceManager, message: &Message) {
        let mut state = self.context.state();

        match message {
            Message::PlaySound {
                path,
                position,
                gain,
                rolloff_factor,
                radius,
            } => {
                let shot_buffer = resource_manager
                    .request_sound_buffer(path, false)
                    .await
                    .unwrap();
                let shot_sound = SpatialSourceBuilder::new(
                    GenericSourceBuilder::new(shot_buffer.into())
                        .with_status(Status::Playing)
                        .with_play_once(true)
                        .with_gain(*gain)
                        .build()
                        .unwrap(),
                )
                .with_position(*position)
                .with_radius(*radius)
                .with_rolloff_factor(*rolloff_factor)
                .build_source();
                let source = state.add_source(shot_sound);
                state
                    .effect_mut(self.reverb)
                    .add_input(EffectInput::direct(source));
            }
            _ => {}
        }
    }
}

impl Visit for SoundManager {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.context.visit("Context", visitor)?;
        self.reverb.visit("Reverb", visitor)?;

        visitor.leave_region()
    }
}

pub struct Level {
    map_root: Handle<Node>,
    pub scene: Handle<Scene>,
    player: Handle<Actor>,
    projectiles: ProjectileContainer,
    pub actors: ActorContainer,
    weapons: WeaponContainer,
    items: ItemContainer,
    spawn_points: Vec<SpawnPoint>,
    sender: Option<Sender<Message>>,
    pub navmesh: Handle<Navmesh>,
    pub control_scheme: Option<Arc<RwLock<ControlScheme>>>,
    death_zones: Vec<DeathZone>,
    time: f32,
    sound_manager: SoundManager,
    proximity_events_receiver: Option<crossbeam::channel::Receiver<ProximityEvent>>,
    contact_events_receiver: Option<crossbeam::channel::Receiver<ContactEvent>>,
    beam: Option<Arc<RwLock<SurfaceSharedData>>>,
}

impl Default for Level {
    fn default() -> Self {
        Self {
            map_root: Default::default(),
            projectiles: ProjectileContainer::new(),
            actors: ActorContainer::new(),
            scene: Default::default(),
            player: Handle::NONE,
            weapons: WeaponContainer::new(),
            items: ItemContainer::new(),
            spawn_points: Default::default(),
            sender: None,
            navmesh: Default::default(),
            control_scheme: None,
            death_zones: Default::default(),
            time: 0.0,
            sound_manager: Default::default(),
            proximity_events_receiver: None,
            contact_events_receiver: None,
            beam: None,
        }
    }
}

impl Visit for Level {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.scene.visit("Scene", visitor)?;
        self.map_root.visit("MapRoot", visitor)?;
        self.player.visit("Player", visitor)?;
        self.actors.visit("Actors", visitor)?;
        self.projectiles.visit("Projectiles", visitor)?;
        self.weapons.visit("Weapons", visitor)?;
        self.spawn_points.visit("SpawnPoints", visitor)?;
        self.death_zones.visit("DeathZones", visitor)?;
        self.time.visit("Time", visitor)?;
        self.sound_manager.visit("SoundManager", visitor)?;
        self.items.visit("Items", visitor)?;
        self.navmesh.visit("Navmesh", visitor)?;

        if visitor.is_reading() {
            self.beam = Some(make_beam());
        }

        visitor.leave_region()
    }
}

pub struct DeathZone {
    bounds: AxisAlignedBoundingBox,
}

impl Visit for DeathZone {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.bounds.visit("Bounds", visitor)?;

        visitor.leave_region()
    }
}

impl Default for DeathZone {
    fn default() -> Self {
        Self {
            bounds: Default::default(),
        }
    }
}

pub struct UpdateContext<'a> {
    pub time: GameTime,
    pub scene: &'a mut Scene,
    pub items: &'a ItemContainer,
    pub navmesh: Handle<Navmesh>,
    pub weapons: &'a WeaponContainer,
}

#[derive(Default)]
pub struct AnalysisResult {
    items: ItemContainer,
    death_zones: Vec<DeathZone>,
    spawn_points: Vec<SpawnPoint>,
    player_spawn_position: Vector3<f32>,
}

fn make_beam() -> Arc<RwLock<SurfaceSharedData>> {
    Arc::new(RwLock::new(SurfaceSharedData::make_cylinder(
        6,
        0.002,
        100.0,
        false,
        UnitQuaternion::from_axis_angle(&Vector3::x_axis(), 90.0f32.to_radians()).to_homogeneous(),
    )))
}

pub async fn analyze(
    scene: &mut Scene,
    resource_manager: ResourceManager,
    sender: Sender<Message>,
) -> AnalysisResult {
    let mut result = AnalysisResult::default();

    let mut items = Vec::new();
    let mut spawn_points = Vec::new();
    let mut death_zones = Vec::new();
    let mut player_spawn_position = Default::default();

    for (handle, node) in scene.graph.pair_iter() {
        let position = node.global_position();
        let name = node.name();
        if name.starts_with("Medkit") {
            items.push((ItemKind::Medkit, position));
        } else if name.starts_with("Ammo_Ak47") {
            items.push((ItemKind::Ak47Ammo, position));
        } else if name.starts_with("Ammo_M4") {
            items.push((ItemKind::M4Ammo, position));
        } else if name.starts_with("Ammo_Plasma") {
            items.push((ItemKind::Plasma, position));
        } else if name.starts_with("Zombie") {
            spawn_points.push(SpawnPoint {
                position: node.global_position(),
                bot_kind: BotKind::Zombie,
                spawned: false,
            })
        } else if name.starts_with("Mutant") {
            spawn_points.push(SpawnPoint {
                position: node.global_position(),
                bot_kind: BotKind::Mutant,
                spawned: false,
            })
        } else if name.starts_with("Parasite") {
            spawn_points.push(SpawnPoint {
                position: node.global_position(),
                bot_kind: BotKind::Parasite,
                spawned: false,
            })
        } else if name.starts_with("PlayerSpawnPoint") {
            player_spawn_position = node.global_position();
        } else if name.starts_with("DeathZone") {
            if let Node::Mesh(_) = node {
                death_zones.push(handle);
            }
        }
    }

    for (kind, position) in items {
        result.items.add(
            Item::new(
                kind,
                position,
                scene,
                resource_manager.clone(),
                sender.clone(),
            )
            .await,
        );
    }
    for handle in death_zones {
        let node = &mut scene.graph[handle];
        node.set_visibility(false);
        result.death_zones.push(DeathZone {
            bounds: node.as_mesh().world_bounding_box(),
        });
    }
    result.spawn_points = spawn_points;
    result.player_spawn_position = player_spawn_position;

    result
}

async fn spawn_player(
    spawn_position: Vector3<f32>,
    actors: &mut ActorContainer,
    weapons: &mut WeaponContainer,
    sender: Sender<Message>,
    resource_manager: ResourceManager,
    control_scheme: Arc<RwLock<ControlScheme>>,
    scene: &mut Scene,
) -> Handle<Actor> {
    let player = Player::new(
        scene,
        resource_manager.clone(),
        spawn_position,
        sender.clone(),
        control_scheme,
    )
    .await;
    let player = actors.add(Actor::Player(player));
    actors
        .get_mut(player)
        .set_position(&mut scene.physics, spawn_position);

    let weapons_to_give = [WeaponKind::M4, WeaponKind::Ak47, WeaponKind::PlasmaRifle];
    for (i, &weapon) in weapons_to_give.iter().enumerate() {
        give_new_weapon(
            weapon,
            player,
            sender.clone(),
            resource_manager.clone(),
            i == weapons_to_give.len() - 1,
            weapons,
            actors,
            scene,
        )
        .await;
    }

    player
}

async fn give_new_weapon(
    kind: WeaponKind,
    actor: Handle<Actor>,
    sender: Sender<Message>,
    resource_manager: ResourceManager,
    visible: bool,
    weapons: &mut WeaponContainer,
    actors: &mut ActorContainer,
    scene: &mut Scene,
) {
    if actors.contains(actor) {
        let mut weapon = Weapon::new(kind, resource_manager, scene, sender.clone()).await;
        weapon.set_owner(actor);
        let weapon_model = weapon.get_model();
        scene.graph[weapon_model].set_visibility(visible);
        let actor = actors.get_mut(actor);
        let weapon_handle = weapons.add(weapon);
        actor.add_weapon(weapon_handle);
        scene.graph.link_nodes(weapon_model, actor.weapon_pivot());
    }
}

async fn spawn_bot(
    spawn_point: &mut SpawnPoint,
    actors: &mut ActorContainer,
    resource_manager: ResourceManager,
    sender: Sender<Message>,
    scene: &mut Scene,
) -> Handle<Actor> {
    spawn_point.spawned = true;

    let bot = add_bot(
        spawn_point.bot_kind,
        spawn_point.position,
        actors,
        resource_manager,
        sender,
        scene,
    )
    .await;

    bot
}

async fn add_bot(
    kind: BotKind,
    position: Vector3<f32>,
    actors: &mut ActorContainer,
    resource_manager: ResourceManager,
    sender: Sender<Message>,
    scene: &mut Scene,
) -> Handle<Actor> {
    let bot = Bot::new(
        kind,
        resource_manager.clone(),
        scene,
        position,
        sender.clone(),
    )
    .await;
    let bot = actors.add(Actor::Bot(bot));

    bot
}

impl Level {
    pub async fn new(
        resource_manager: ResourceManager,
        control_scheme: Arc<RwLock<ControlScheme>>,
        sender: Sender<Message>,
    ) -> (Level, Scene) {
        let mut scene = Scene::new();

        let sound_manager = SoundManager::new(scene.sound_context.clone());

        let (proximity_events_sender, proximity_events_receiver) = crossbeam::channel::unbounded();
        let (contact_events_sender, contact_events_receiver) = crossbeam::channel::unbounded();

        scene.physics.event_handler = Box::new(ChannelEventCollector::new(
            proximity_events_sender.clone(),
            contact_events_sender.clone(),
        ));

        let map_model = resource_manager
            .request_model(Path::new("data/levels/arrival.rgs"))
            .await
            .unwrap();

        // Instantiate map
        let map_root = map_model.instantiate_geometry(&mut scene);

        let AnalysisResult {
            items,
            death_zones,
            mut spawn_points,
            player_spawn_position,
        } = analyze(&mut scene, resource_manager.clone(), sender.clone()).await;
        let mut actors = ActorContainer::new();
        let mut weapons = WeaponContainer::new();

        for pt in spawn_points.iter_mut() {
            spawn_bot(
                pt,
                &mut actors,
                resource_manager.clone(),
                sender.clone(),
                &mut scene,
            )
            .await;
        }

        let level = Level {
            player: spawn_player(
                player_spawn_position,
                &mut actors,
                &mut weapons,
                sender.clone(),
                resource_manager.clone(),
                control_scheme.clone(),
                &mut scene,
            )
            .await,
            map_root,
            actors,
            weapons,
            items,
            death_zones,
            spawn_points,
            navmesh: scene.navmeshes.handle_from_index(0),
            scene: Handle::NONE, // Filled when scene will be moved to engine.
            sender: Some(sender),
            control_scheme: Some(control_scheme),
            time: 0.0,
            contact_events_receiver: Some(contact_events_receiver),
            proximity_events_receiver: Some(proximity_events_receiver),
            projectiles: ProjectileContainer::new(),
            sound_manager,
            beam: Some(make_beam()),
        };

        (level, scene)
    }

    pub fn destroy(&mut self, engine: &mut GameEngine) {
        engine.scenes.remove(self.scene);
    }

    async fn give_new_weapon(
        &mut self,
        engine: &mut GameEngine,
        actor: Handle<Actor>,
        kind: WeaponKind,
    ) {
        give_new_weapon(
            kind,
            actor,
            self.sender.clone().unwrap(),
            engine.resource_manager.clone(),
            true,
            &mut self.weapons,
            &mut self.actors,
            &mut engine.scenes[self.scene],
        )
        .await;
    }

    pub fn get_player(&self) -> Handle<Actor> {
        self.player
    }

    pub fn process_input_event(&mut self, event: &Event<()>, scene: &mut Scene, dt: f32) {
        if self.player.is_some() {
            if let Actor::Player(player) = self.actors.get_mut(self.player) {
                player.process_input_event(event, dt, scene);
            }
        }
    }

    pub fn actors(&self) -> &ActorContainer {
        &self.actors
    }

    pub fn actors_mut(&mut self) -> &mut ActorContainer {
        &mut self.actors
    }

    pub fn weapons(&self) -> &WeaponContainer {
        &self.weapons
    }

    fn pick(&self, engine: &mut GameEngine, from: Vector3<f32>, to: Vector3<f32>) -> Vector3<f32> {
        let scene = &mut engine.scenes[self.scene];
        if let Some(ray) = Ray::from_two_points(&from, &to) {
            let options = RayCastOptions {
                ray,
                max_len: std::f32::MAX,
                groups: InteractionGroups::all(),
                sort_results: true,
            };
            let mut query_buffer = Vec::default();
            scene.physics.cast_ray(options, &mut query_buffer);
            if let Some(pt) = query_buffer.first() {
                pt.position.coords
            } else {
                from
            }
        } else {
            from
        }
    }

    fn remove_weapon(&mut self, engine: &mut GameEngine, weapon: Handle<Weapon>) {
        for projectile in self.projectiles.iter_mut() {
            if projectile.owner == weapon {
                // Reset owner because handle to weapon will be invalid after weapon freed.
                projectile.owner = Handle::NONE;
            }
        }
        self.weapons[weapon].clean_up(&mut engine.scenes[self.scene]);
        self.weapons.free(weapon);
    }

    async fn add_bot(
        &mut self,
        engine: &mut GameEngine,
        kind: BotKind,
        position: Vector3<f32>,
    ) -> Handle<Actor> {
        add_bot(
            kind,
            position,
            &mut self.actors,
            engine.resource_manager.clone(),
            self.sender.clone().unwrap(),
            &mut engine.scenes[self.scene],
        )
        .await
    }

    async fn remove_actor(&mut self, engine: &mut GameEngine, actor: Handle<Actor>) {
        if self.actors.contains(actor) {
            let scene = &mut engine.scenes[self.scene];
            let character = self.actors.get(actor);

            // Make sure to remove weapons and drop appropriate items (items will be temporary).
            let drop_position = character.position(&scene.physics);
            let weapons = character
                .weapons()
                .iter()
                .copied()
                .collect::<Vec<Handle<Weapon>>>();
            for weapon in weapons {
                let item_kind = match self.weapons[weapon].get_kind() {
                    WeaponKind::M4 => ItemKind::M4,
                    WeaponKind::Ak47 => ItemKind::Ak47,
                    WeaponKind::PlasmaRifle => ItemKind::PlasmaGun,
                };
                self.spawn_item(engine, item_kind, drop_position, true)
                    .await;
                self.remove_weapon(engine, weapon);
            }

            let scene = &mut engine.scenes[self.scene];
            self.actors.get_mut(actor).clean_up(scene);
            self.actors.free(actor);

            if self.player == actor {
                self.player = Handle::NONE;
            }
        }
    }

    async fn give_item(&mut self, engine: &mut GameEngine, actor: Handle<Actor>, kind: ItemKind) {
        if self.actors.contains(actor) {
            let character = self.actors.get_mut(actor);
            match kind {
                ItemKind::Medkit => character.heal(20.0),
                ItemKind::Ak47 | ItemKind::PlasmaGun | ItemKind::M4 | ItemKind::RocketLauncher => {
                    let weapon_kind = match kind {
                        ItemKind::Ak47 => WeaponKind::Ak47,
                        ItemKind::PlasmaGun => WeaponKind::PlasmaRifle,
                        ItemKind::M4 => WeaponKind::M4,
                        _ => unreachable!(),
                    };

                    let mut found = false;
                    for weapon_handle in character.weapons() {
                        let weapon = &mut self.weapons[*weapon_handle];
                        // If actor already has weapon of given kind, then just add ammo to it.
                        if weapon.get_kind() == weapon_kind {
                            found = true;
                            weapon.add_ammo(200);
                            break;
                        }
                    }
                    // Finally if actor does not have such weapon, give new one to him.
                    if !found {
                        self.give_new_weapon(engine, actor, weapon_kind).await;
                    }
                }
                ItemKind::Plasma | ItemKind::Ak47Ammo | ItemKind::M4Ammo => {
                    for weapon in character.weapons() {
                        let weapon = &mut self.weapons[*weapon];
                        let (weapon_kind, ammo) = match kind {
                            ItemKind::Plasma => (WeaponKind::PlasmaRifle, 200),
                            ItemKind::Ak47Ammo => (WeaponKind::Ak47, 200),
                            ItemKind::M4Ammo => (WeaponKind::M4, 200),
                            _ => continue,
                        };
                        if weapon.get_kind() == weapon_kind {
                            weapon.add_ammo(ammo);
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn pickup_item(
        &mut self,
        engine: &mut GameEngine,
        actor: Handle<Actor>,
        item: Handle<Item>,
    ) {
        if self.actors.contains(actor) && self.items.contains(item) {
            let item = self.items.get_mut(item);

            let scene = &mut engine.scenes[self.scene];
            let position = item.position(&scene.graph);
            let kind = item.get_kind();
            self.sender
                .as_ref()
                .unwrap()
                .send(Message::PlaySound {
                    path: PathBuf::from("data/sounds/item_pickup.ogg"),
                    position,
                    gain: 1.0,
                    rolloff_factor: 3.0,
                    radius: 2.0,
                })
                .unwrap();
            self.give_item(engine, actor, kind).await;
        }
    }

    async fn create_projectile(
        &mut self,
        engine: &mut GameEngine,
        kind: ProjectileKind,
        position: Vector3<f32>,
        direction: Vector3<f32>,
        initial_velocity: Vector3<f32>,
        owner: Handle<Weapon>,
    ) {
        let scene = &mut engine.scenes[self.scene];
        let projectile = Projectile::new(
            kind,
            engine.resource_manager.clone(),
            scene,
            direction,
            position,
            owner,
            initial_velocity,
            self.sender.as_ref().unwrap().clone(),
        )
        .await;
        self.projectiles.add(projectile);
    }

    async fn shoot_weapon(
        &mut self,
        engine: &mut GameEngine,
        weapon_handle: Handle<Weapon>,
        time: GameTime,
        direction: Option<Vector3<f32>>,
    ) {
        if self.weapons.contains(weapon_handle) {
            let scene = &mut engine.scenes[self.scene];
            let weapon = &mut self.weapons[weapon_handle];
            weapon.try_shoot(
                weapon_handle,
                scene,
                time,
                engine.resource_manager.clone(),
                direction,
            );
        }
    }

    fn show_weapon(&mut self, engine: &mut GameEngine, weapon_handle: Handle<Weapon>, state: bool) {
        self.weapons[weapon_handle].set_visibility(state, &mut engine.scenes[self.scene].graph)
    }

    fn damage_actor(
        &mut self,
        engine: &GameEngine,
        actor_handle: Handle<Actor>,
        who: Handle<Actor>,
        amount: f32,
    ) {
        if self.actors.contains(actor_handle)
            && (who.is_none() || who.is_some() && self.actors.contains(who))
        {
            let who_position = if who.is_some() {
                let scene = &engine.scenes[self.scene];
                Some(self.actors.get(who).position(&scene.physics))
            } else {
                None
            };
            let actor = self.actors.get_mut(actor_handle);
            if let Actor::Bot(bot) = actor {
                if let Some(who_position) = who_position {
                    bot.set_target(actor_handle, who_position);
                }
            }
            actor.damage(amount);
        }
    }

    async fn spawn_item(
        &mut self,
        engine: &mut GameEngine,
        kind: ItemKind,
        position: Vector3<f32>,
        adjust_height: bool,
    ) {
        let position = if adjust_height {
            self.pick(engine, position, position - Vector3::new(0.0, 1000.0, 0.0))
        } else {
            position
        };
        let scene = &mut engine.scenes[self.scene];
        let item = Item::new(
            kind,
            position,
            scene,
            engine.resource_manager.clone(),
            self.sender.as_ref().unwrap().clone(),
        )
        .await;
        self.items.add(item);
    }

    fn update_death_zones(&mut self, scene: &Scene) {
        for (handle, actor) in self.actors.pair_iter_mut() {
            for death_zone in self.death_zones.iter() {
                if death_zone
                    .bounds
                    .is_contains_point(actor.position(&scene.physics))
                {
                    self.sender
                        .as_ref()
                        .unwrap()
                        .send(Message::DamageActor {
                            actor: handle,
                            who: Default::default(),
                            amount: 99999.0,
                        })
                        .unwrap();
                }
            }
        }
    }

    fn update_game_ending(&self) {
        if self.actors.get(self.player).is_dead() {
            self.sender
                .as_ref()
                .unwrap()
                .send(Message::EndMatch)
                .unwrap();
        }
    }

    pub fn update(&mut self, engine: &mut GameEngine, time: GameTime) {
        self.time += time.delta;
        let scene = &mut engine.scenes[self.scene];
        while let Ok(proximity_event) = self.proximity_events_receiver.as_ref().unwrap().try_recv()
        {
            for proj in self.projectiles.iter_mut() {
                proj.handle_proximity(&proximity_event, scene, &self.actors, &self.weapons);
            }
        }
        self.update_death_zones(scene);
        self.weapons.update(scene, time.delta);
        self.projectiles
            .update(scene, &self.actors, &self.weapons, time);
        let mut ctx = UpdateContext {
            time,
            scene,
            items: &self.items,
            navmesh: self.navmesh,
            weapons: &self.weapons,
        };
        self.actors.update(&mut ctx);
        self.update_game_ending();
    }

    pub async fn handle_message(
        &mut self,
        engine: &mut GameEngine,
        message: &Message,
        time: GameTime,
    ) {
        self.sound_manager
            .handle_message(engine.resource_manager.clone(), &message)
            .await;

        match message {
            &Message::GiveNewWeapon { actor, kind } => {
                self.give_new_weapon(engine, actor, kind).await;
            }
            Message::AddBot { kind, position } => {
                self.add_bot(engine, *kind, *position).await;
            }
            &Message::RemoveActor { actor } => self.remove_actor(engine, actor).await,
            &Message::GiveItem { actor, kind } => {
                self.give_item(engine, actor, kind).await;
            }
            &Message::PickUpItem { actor, item } => {
                self.pickup_item(engine, actor, item).await;
            }
            &Message::ShootWeapon { weapon, direction } => {
                self.shoot_weapon(engine, weapon, time, direction).await
            }
            &Message::CreateProjectile {
                kind,
                position,
                direction,
                initial_velocity,
                owner,
            } => {
                self.create_projectile(engine, kind, position, direction, initial_velocity, owner)
                    .await
            }
            &Message::ShowWeapon { weapon, state } => self.show_weapon(engine, weapon, state),
            &Message::SpawnBot { spawn_point_id } => {
                if let Some(spawn_point) = self.spawn_points.get_mut(spawn_point_id) {
                    spawn_bot(
                        spawn_point,
                        &mut self.actors,
                        engine.resource_manager.clone(),
                        self.sender.clone().unwrap(),
                        &mut engine.scenes[self.scene],
                    )
                    .await;
                }
            }
            &Message::DamageActor { actor, who, amount } => {
                self.damage_actor(engine, actor, who, amount);
            }
            &Message::CreateEffect {
                kind,
                position,
                orientation,
            } => {
                effects::create(
                    kind,
                    &mut engine.scenes[self.scene].graph,
                    engine.resource_manager.clone(),
                    position,
                    orientation,
                );
            }
            &Message::SpawnItem {
                kind,
                position,
                adjust_height,
            } => self.spawn_item(engine, kind, position, adjust_height).await,
            Message::ShootRay {
                weapon,
                begin,
                end,
                damage,
                impact_sound,
            } => {
                let scene = &mut engine.scenes[self.scene];

                MeshBuilder::new(
                    BaseBuilder::new().with_lifetime(0.7).with_local_transform(
                        TransformBuilder::new()
                            .with_local_position(*begin)
                            .with_local_rotation(UnitQuaternion::face_towards(
                                &(end - begin),
                                &Vector3::y(),
                            ))
                            .build(),
                    ),
                )
                .with_surfaces(vec![SurfaceBuilder::new(self.beam.clone().unwrap())
                    .with_color(Color::from_rgba(255, 127, 40, 120))
                    .build()])
                .with_cast_shadows(false)
                .with_render_path(RenderPath::Forward)
                .build(&mut scene.graph);

                // Do immediate intersection test and solve it.
                if let Some(hit) = ray_hit(
                    *begin,
                    *end,
                    *weapon,
                    &self.weapons,
                    &self.actors,
                    &mut scene.physics,
                    Default::default(),
                ) {
                    // Just send new messages, instead of doing everything manually here.
                    self.sender
                        .as_ref()
                        .unwrap()
                        .send(Message::CreateEffect {
                            kind: if hit.actor.is_some() {
                                EffectKind::BloodSpray
                            } else {
                                EffectKind::BulletImpact
                            },
                            position: hit.position,
                            orientation: UnitQuaternion::face_towards(&hit.normal, &Vector3::y()),
                        })
                        .unwrap();

                    self.sender
                        .as_ref()
                        .unwrap()
                        .send(Message::PlaySound {
                            path: impact_sound.clone(),
                            position: hit.position,
                            gain: 1.0,
                            rolloff_factor: 4.0,
                            radius: 3.0,
                        })
                        .unwrap();

                    self.sender
                        .as_ref()
                        .unwrap()
                        .send(Message::DamageActor {
                            actor: hit.actor,
                            who: hit.who,
                            amount: *damage,
                        })
                        .unwrap();
                }
            }
            _ => (),
        }
    }

    pub fn set_message_sender(&mut self, sender: Sender<Message>, engine: &mut GameEngine) {
        self.sender = Some(sender.clone());

        // Attach new sender to all event sources.
        for actor in self.actors.iter_mut() {
            actor.sender = Some(sender.clone());
        }
        for weapon in self.weapons.iter_mut() {
            weapon.sender = Some(sender.clone());
        }
        for projectile in self.projectiles.iter_mut() {
            projectile.sender = Some(sender.clone());
        }
        for item in self.items.iter_mut() {
            item.sender = Some(sender.clone());
        }

        let (proximity_events_sender, proximity_events_receiver) = crossbeam::channel::unbounded();
        let (contact_events_sender, contact_events_receiver) = crossbeam::channel::unbounded();

        self.proximity_events_receiver = Some(proximity_events_receiver);
        self.contact_events_receiver = Some(contact_events_receiver);

        engine.scenes[self.scene].physics.event_handler = Box::new(ChannelEventCollector::new(
            proximity_events_sender.clone(),
            contact_events_sender.clone(),
        ));
    }

    pub fn debug_draw(&self, engine: &mut GameEngine) {
        let scene = &mut engine.scenes[self.scene];

        let drawing_context = &mut scene.drawing_context;

        drawing_context.clear_lines();

        scene.physics.draw(drawing_context);

        if self.navmesh.is_some() {
            let navmesh = &scene.navmeshes[self.navmesh];

            for pt in navmesh.vertices() {
                for neighbour in pt.neighbours() {
                    drawing_context.add_line(scene::Line {
                        begin: pt.position(),
                        end: navmesh.vertices()[*neighbour as usize].position(),
                        color: Default::default(),
                    });
                }
            }

            for actor in self.actors.iter() {
                if let Actor::Bot(bot) = actor {
                    bot.debug_draw(drawing_context);
                }
            }
        }

        for death_zone in self.death_zones.iter() {
            drawing_context.draw_aabb(&death_zone.bounds, Color::opaque(0, 0, 200));
        }
    }
}

pub struct SpawnPoint {
    position: Vector3<f32>,
    bot_kind: BotKind,
    spawned: bool,
}

impl Default for SpawnPoint {
    fn default() -> Self {
        Self {
            position: Default::default(),
            bot_kind: BotKind::Zombie,
            spawned: false,
        }
    }
}

impl Visit for SpawnPoint {
    fn visit(&mut self, name: &str, visitor: &mut Visitor) -> VisitResult {
        visitor.enter_region(name)?;

        self.position.visit("Position", visitor)?;
        self.spawned.visit("Spawned", visitor)?;

        let mut kind_id = self.bot_kind.id();
        kind_id.visit("BotKind", visitor)?;
        if visitor.is_reading() {
            self.bot_kind = BotKind::from_id(kind_id)?;
        }

        visitor.leave_region()
    }
}
