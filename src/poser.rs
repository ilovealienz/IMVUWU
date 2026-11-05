//! Independent full-avatar pose workspace; all pose transforms stay in IMVU coordinates.
use crate::{
    model::V,
    pose::{Animation, Pose, PoseKey, PoseTrack, Quat, Skeleton},
    pose_render::{view_point, AvatarMesh},
    render::Camera,
};
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
};
use xmltree::{Element, XMLNode};

#[derive(Clone)]
struct Keyframe {
    time: f32,
    pose: Pose,
}
#[derive(Clone)]
struct Actor {
    pose: Pose,
    reference: Pose,
    angles: Vec<V>,
    keys: Vec<Keyframe>,
    variant: usize,
}
impl Actor {
    fn new(s: &Skeleton) -> Self {
        let pose = s.rest_pose();
        Self {
            reference: pose.clone(),
            angles: vec![V::default(); pose.local.len()],
            pose,
            keys: Vec::new(),
            variant: 0,
        }
    }
}
#[derive(Clone)]
struct Scene {
    actors: [Actor; 2],
    couple: bool,
    duration: f32,
    pair_offsets: [f32; 2],
}
impl Scene {
    fn new(s: &Skeleton) -> Self {
        let mut actors = [Actor::new(s), Actor::new(s)];
        actors[1].variant = 1;
        Self {
            actors,
            couple: false,
            duration: 10.0,
            pair_offsets: [0.0; 2],
        }
    }
    fn shift_actor(&mut self, index: usize, delta: f32) {
        let a = &mut self.actors[index];
        a.pose.local[1].position.0 += delta;
        a.reference.local[1].position.0 += delta;
        for k in &mut a.keys {
            k.pose.local[1].position.0 += delta;
        }
    }
    fn set_couple(&mut self, enabled: bool) {
        if enabled == self.couple {
            return;
        }
        if enabled {
            self.pair_offsets = [0.0; 2];
            if (self.actors[0].pose.local[1].position.0 - self.actors[1].pose.local[1].position.0)
                .abs()
                < 1.0
            {
                for (i, x) in [(0, -180.0), (1, 180.0)] {
                    let delta = x - self.actors[i].pose.local[1].position.0;
                    self.pair_offsets[i] = delta;
                    self.shift_actor(i, delta);
                }
            }
        } else {
            for i in 0..2 {
                self.shift_actor(i, -self.pair_offsets[i]);
            }
            self.pair_offsets = [0.0; 2];
        }
        self.couple = enabled;
    }
}
#[derive(Clone)]
struct History {
    scene: Scene,
    revision: u64,
}
#[derive(Clone, Copy)]
enum Pending {
    Reset,
    Import,
    Open,
}
enum Drag {
    Translate {
        start: V,
        direction: V,
        per_unit: Vec2,
        delta: Vec2,
    },
    Orbit,
    Pan,
    Rotate {
        axis: usize,
        start: V,
        delta: Vec2,
    },
    Move {
        start: V,
        delta: Vec2,
    },
}
const FLASH_JOINT_AXES: [u8; 87] = [
    7, 7, 0, 7, 4, 6, 4, 0, 0, 7, 4, 6, 4, 0, 7, 7, 7, 7, 7, 7, 7, 7, 7, 0, 7, 7, 1, 2, 1, 6, 0, 6,
    4, 4, 0, 0, 6, 4, 4, 0, 0, 6, 4, 4, 0, 0, 6, 4, 4, 0, 0, 6, 4, 4, 0, 7, 7, 1, 2, 1, 6, 0, 6, 4,
    4, 0, 0, 6, 4, 4, 0, 0, 6, 4, 4, 0, 0, 6, 4, 4, 0, 0, 6, 4, 4, 0, 0,
];
// The Flash preview swapped native Y/Z. Its constraint letters are preview axes,
// so convert them before applying rotations in the native IMVU skeleton.
fn native_joint_axes(bone: usize) -> u8 {
    let m = FLASH_JOINT_AXES[bone];
    (m & 1) | ((m & 2) << 1) | ((m & 4) >> 1)
}
// Mirrored elbow bind frames need opposite signed Z rotations for positive bend.
pub(crate) fn joint_rotation(bone: usize, mut angles: V, free: bool) -> Quat {
    if !free && bone == 27 {
        angles.2 = -angles.2;
    }
    Quat::from_euler_degrees(angles)
}
fn rotation_label(bone: usize, axis: usize, free: bool) -> &'static str {
    match (bone, axis, free) {
        (27 | 58, 2, false) => "Elbow bend (Z)",
        (4 | 10, 1, false) => "Knee bend (Y)",
        (_, 0, _) => "Rotate X",
        (_, 1, _) => "Rotate Y",
        _ => "Rotate Z",
    }
}
pub struct AvatarPoser {
    pub(crate) gpu: Option<crate::gpu::Shared>,
    pub(crate) use_gpu: bool,
    gpu_scene: Option<std::sync::Arc<crate::gpu::Scene>>,
    skeleton: Skeleton,
    meshes: [AvatarMesh; 2],
    scene: Scene,
    active: usize,
    selected: usize,
    camera: Camera,
    preview: Option<egui::TextureHandle>,
    projected: Vec<[f32; 3]>,
    redraw: bool,
    joints: bool,
    fingers: bool,
    all_bones: bool,
    move_gizmo: bool,
    drag: Option<Drag>,
    axis: usize,
    free_joints: bool,
    filter: String,
    status: String,
    time: f32,
    playing: bool,
    undo: Vec<History>,
    redo: Vec<History>,
    revision: u64,
    next_revision: u64,
    saved_revision: u64,
    preview_budget: crate::render::PreviewBudget,
    render_size: [usize; 2],
    project_path: Option<PathBuf>,
    pending: Option<Pending>,
    slider_editing: bool,
    /// True while a rotation slider or its number box is being dragged or
    /// typed in this frame; the viewport then hides the rotation arrows so the
    /// joint itself is easy to see.
    sliders_in_use: bool,
}
impl AvatarPoser {
    pub fn new() -> Self {
        let skeleton = Skeleton::bundled();
        let scene = Scene::new(&skeleton);
        let selected = skeleton.find("Head").unwrap_or(22);
        Self {
            gpu: None,
            use_gpu: false,
            gpu_scene: None,
            skeleton,
            meshes: [AvatarMesh::female(), AvatarMesh::male()],
            scene,
            active: 0,
            selected,
            camera: Camera {
                zoom: 0.8,
                ..Default::default()
            },
            preview: None,
            projected: Vec::new(),
            redraw: true,
            joints: true,
            fingers: false,
            all_bones: false,
            move_gizmo: false,
            drag: None,
            axis: 0,
            free_joints: false,
            filter: String::new(),
            status: if crate::avatars::load(true).is_none() || crate::avatars::load(false).is_none()
            {
                "Showing the plain preview avatar: build IMVU's avatars in Settings › IMVU \
                 avatars for the real ones. Posing and XAF export work either way."
                    .into()
            } else {
                "Click a joint, then drag or use the rotation sliders. Export XAF when ready."
                    .into()
            },
            time: 0.0,
            playing: false,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            next_revision: 1,
            saved_revision: 0,
            preview_budget: Default::default(),
            render_size: [0, 0],
            project_path: None,
            pending: None,
            slider_editing: false,
            sliders_in_use: false,
        }
    }
    pub(crate) fn invalidate(&mut self) {
        self.redraw = true;
    }
    /// Reloads the female/male avatar meshes after they are built.
    pub fn reload_avatars(&mut self) {
        self.meshes = [AvatarMesh::female(), AvatarMesh::male()];
        self.gpu_scene = None;
        self.redraw = true;
    }
    pub fn dirty(&self) -> bool {
        self.revision != self.saved_revision
    }
    fn allowed(&self, bone: usize, axis: usize) -> bool {
        self.free_joints || (native_joint_axes(bone) & (1 << axis)) != 0
    }
    fn select(&mut self, actor: usize, bone: usize) {
        self.active = actor;
        self.selected = bone;
        if !self.allowed(bone, self.axis) {
            self.axis = (0..3).find(|&i| self.allowed(bone, i)).unwrap_or(0);
        }
    }
    pub fn suspend(&mut self) {
        self.playing = false;
        self.drag = None;
    }
    fn checkpoint(&mut self) {
        self.undo.push(History {
            scene: self.scene.clone(),
            revision: self.revision,
        });
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.revision = self.next_revision;
        self.next_revision += 1;
        self.playing = false;
    }
    fn undo(&mut self, redo: bool) {
        if let Some(h) = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        } {
            let current = History {
                scene: std::mem::replace(&mut self.scene, h.scene),
                revision: self.revision,
            };
            if redo {
                self.undo.push(current)
            } else {
                self.redo.push(current)
            }
            self.revision = h.revision;
            self.redraw = true;
            self.playing = false;
            self.drag = None;
        }
    }
    fn apply_angles(&mut self, angles: V) {
        let actor = &mut self.scene.actors[self.active];
        actor.angles[self.selected] = angles;
        actor.pose.local[self.selected].rotation = actor.reference.local[self.selected]
            .rotation
            .mul(joint_rotation(self.selected, angles, self.free_joints))
            .normalized();
        self.autokey();
        self.redraw = true;
    }
    fn set_avatar_position(&mut self, position: V) {
        self.scene.actors[self.active].pose.local[1].position = position;
        self.autokey();
        self.redraw = true;
    }
    fn autokey(&mut self) {
        if !self.scene.actors[self.active].keys.is_empty() {
            self.set_key();
        }
    }
    fn set_key(&mut self) {
        let actor = &mut self.scene.actors[self.active];
        let pose = actor.pose.clone();
        if actor.keys.len() >= 500
            && !actor
                .keys
                .iter()
                .any(|k| (k.time - self.time).abs() < 0.0001)
        {
            self.status = "Maximum 500 key poses per avatar reached.".into();
            return;
        }
        if let Some(k) = actor
            .keys
            .iter_mut()
            .find(|k| (k.time - self.time).abs() < 0.0001)
        {
            k.pose = pose
        } else {
            actor.keys.push(Keyframe {
                time: self.time,
                pose,
            });
            actor.keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        }
    }
    fn sample_keys(keys: &[Keyframe], time: f32) -> Option<Pose> {
        let first = keys.first()?;
        if time <= first.time {
            return Some(first.pose.clone());
        }
        if let Some(pair) = keys.windows(2).find(|p| time <= p[1].time) {
            let f = (time - pair[0].time) / (pair[1].time - pair[0].time);
            let mut pose = pair[0].pose.clone();
            for (t, b) in pose.local.iter_mut().zip(&pair[1].pose.local) {
                t.position = t.position.lerp(b.position, f);
                t.rotation = t.rotation.slerp(b.rotation, f);
            }
            Some(pose)
        } else {
            Some(keys.last()?.pose.clone())
        }
    }
    fn sample_time(&mut self) {
        for actor in &mut self.scene.actors {
            if let Some(p) = Self::sample_keys(&actor.keys, self.time) {
                actor.pose = p.clone();
                actor.reference = p;
                actor.angles.fill(V::default());
            }
        }
        self.redraw = true;
    }
    fn clip(&self, actor: usize, full: bool) -> Animation {
        let actor = &self.scene.actors[actor];
        let rest = self.skeleton.rest_pose();
        let mut tracks = Vec::new();
        for bone in self.skeleton.control_ids() {
            let mut keys: Vec<PoseKey> = if actor.keys.is_empty() {
                vec![
                    PoseKey {
                        time: 0.0,
                        transform: actor.pose.local[bone],
                    },
                    PoseKey {
                        time: self.scene.duration,
                        transform: actor.pose.local[bone],
                    },
                ]
            } else {
                actor
                    .keys
                    .iter()
                    .map(|k| PoseKey {
                        time: k.time,
                        transform: k.pose.local[bone],
                    })
                    .collect()
            };
            if keys[0].time > 0.0 {
                keys.insert(
                    0,
                    PoseKey {
                        time: 0.0,
                        transform: keys[0].transform,
                    },
                );
            }
            if keys.last().unwrap().time < self.scene.duration {
                keys.push(PoseKey {
                    time: self.scene.duration,
                    transform: keys.last().unwrap().transform,
                });
            }
            if full
                || keys.iter().any(|k| {
                    k.transform.position.sub(rest.local[bone].position).len() > 0.0001
                        || k.transform.rotation.dot(rest.local[bone].rotation).abs() < 0.99999
                })
            {
                tracks.push(PoseTrack { bone, keys });
            }
        }
        Animation {
            duration: self.scene.duration,
            tracks,
        }
    }
    fn request(&mut self, p: Pending) {
        if self.dirty() {
            self.pending = Some(p)
        } else {
            self.perform(p)
        }
    }
    fn perform(&mut self, p: Pending) {
        match p {
            Pending::Reset => {
                self.checkpoint();
                let variant = self.scene.actors[self.active].variant;
                self.scene.actors[self.active] = Actor::new(&self.skeleton);
                self.scene.actors[self.active].variant = variant;
                if self.scene.couple {
                    self.scene
                        .shift_actor(self.active, self.scene.pair_offsets[self.active]);
                }
                self.time = 0.0;
                self.redraw = true;
                self.status = "Avatar pose reset.".into();
            }
            Pending::Import => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("IMVU skeletal animation", &["xaf"])
                    .pick_file()
                {
                    let result = read_bounded(&path, 16 * 1024 * 1024)
                        .and_then(|b| Animation::load(&self.skeleton, &b));
                    match result {
                        Ok(animation) => {
                            let times: std::collections::BTreeSet<_> = animation
                                .tracks
                                .iter()
                                .flat_map(|t| t.keys.iter().map(|k| k.time.to_bits()))
                                .collect();
                            if times.len() > 500 {
                                self.status="This animation has more than 500 distinct key times; simplify it before loading.".into();
                                return;
                            }
                            let mut keys: Vec<_> = times
                                .into_iter()
                                .map(|t| {
                                    let time = f32::from_bits(t);
                                    Keyframe {
                                        time,
                                        pose: animation.sample(&self.skeleton, time),
                                    }
                                })
                                .collect();
                            keys.sort_by(|a, b| a.time.total_cmp(&b.time));
                            let mut pose = animation.sample(&self.skeleton, 0.0);
                            bake_root(&self.skeleton, &mut pose);
                            for k in &mut keys {
                                bake_root(&self.skeleton, &mut k.pose);
                            }
                            if static_clip(&animation) {
                                keys.clear();
                            }
                            self.checkpoint();
                            self.scene.duration = animation.duration.max(
                                self.scene.actors[1 - self.active]
                                    .keys
                                    .last()
                                    .map_or(0.0, |k| k.time),
                            );
                            let variant = self.scene.actors[self.active].variant;
                            self.scene.pair_offsets[self.active] = if self.scene.couple {
                                pose.local[1].position.0 - self.skeleton.bones[1].translation.0
                            } else {
                                0.0
                            };
                            self.scene.actors[self.active] = Actor {
                                reference: pose.clone(),
                                angles: vec![V::default(); pose.local.len()],
                                pose,
                                keys,
                                variant,
                            };
                            self.time = 0.0;
                            self.redraw = true;
                            self.status=format!("Loaded {}. Rotate a joint to adjust this pose, or scrub the animation.",path.display());
                        }
                        Err(e) => self.status = format!("Could not open XAF: {e}"),
                    }
                }
            }
            Pending::Open => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Avatar pose project", &["xpose"])
                    .pick_file()
                {
                    match read_bounded(&path, 32 * 1024 * 1024)
                        .and_then(|b| load_scene(&self.skeleton, &b))
                    {
                        Ok(scene) => {
                            self.scene = scene;
                            self.active = 0;
                            self.time = 0.0;
                            self.undo.clear();
                            self.redo.clear();
                            self.revision = self.next_revision;
                            self.next_revision += 1;
                            self.saved_revision = self.revision;
                            self.project_path = Some(path);
                            self.playing = false;
                            self.redraw = true;
                            self.status = "Pose project loaded.".into();
                        }
                        Err(e) => self.status = format!("Could not open project: {e}"),
                    }
                }
            }
        }
    }
    fn project_bytes(&self) -> Result<Vec<u8>, String> {
        let mut root = Element::new("AVATARPOSE");
        root.attributes.insert("VERSION".into(), "1".into());
        root.attributes
            .insert("COUPLE".into(), u8::from(self.scene.couple).to_string());
        for actor in 0..2 {
            let mut node = Element::new("ACTOR");
            node.attributes
                .insert("MODEL".into(), self.scene.actors[actor].variant.to_string());
            node.attributes.insert(
                "PAIR_OFFSET_X".into(),
                self.scene.pair_offsets[actor].to_string(),
            );
            node.attributes.insert(
                "MODE".into(),
                if self.scene.actors[actor].keys.is_empty() {
                    "held"
                } else {
                    "animated"
                }
                .into(),
            );
            node.children.push(XMLNode::CData(
                self.clip(actor, true).export(&self.skeleton)?,
            ));
            root.children.push(XMLNode::Element(node));
        }
        let mut bytes = Vec::new();
        root.write_with_config(
            &mut bytes,
            xmltree::EmitterConfig::new().perform_indent(true),
        )
        .map_err(|e| e.to_string())?;
        load_scene(&self.skeleton, &bytes)?;
        Ok(bytes)
    }
    pub fn save_project(&mut self) -> bool {
        let result = (|| -> Result<Option<PathBuf>, String> {
            let bytes = self.project_bytes()?;
            let mut dialog = rfd::FileDialog::new()
                .add_filter("Avatar pose project", &["xpose"])
                .set_file_name("AvatarPose.xpose");
            if let Some(p) = &self.project_path {
                dialog = dialog.set_file_name(p.file_name().unwrap_or_default().to_string_lossy());
                if let Some(folder) = p.parent() {
                    dialog = dialog.set_directory(folder)
                }
            }
            let Some(mut path) = dialog.save_file() else {
                return Ok(None);
            };
            if path.extension().is_none() {
                path.set_extension("xpose");
            }
            atomic_write(&path, &bytes)?;
            Ok(Some(path))
        })();
        match result {
            Ok(Some(path)) => {
                self.saved_revision = self.revision;
                self.status = format!("Saved project {}", path.display());
                self.project_path = Some(path);
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.status = format!("Could not save project: {e}");
                false
            }
        }
    }
    fn export(&mut self) {
        let result = (|| -> Result<Option<String>, String> {
            if self.scene.couple {
                let Some(folder) = rfd::FileDialog::new()
                    .set_title("Choose a folder for the two XAF files")
                    .pick_folder()
                else {
                    return Ok(None);
                };
                let a = self.clip(0, true).export(&self.skeleton)?;
                let b = self.clip(1, true).export(&self.skeleton)?;
                let (pa, pb) = (folder.join("Avatar-A.xaf"), folder.join("Avatar-B.xaf"));
                if pa.exists() || pb.exists() {
                    return Err("Avatar-A.xaf or Avatar-B.xaf already exists in that folder. Choose a new folder to keep the previous files.".into());
                }
                atomic_write(&pa, a.as_bytes())?;
                atomic_write(&pb, b.as_bytes())?;
                Ok(Some(format!("Exported Avatar-A.xaf and Avatar-B.xaf to {}. Use a matching two-seat product for the pair.",folder.display())))
            } else {
                let Some(mut path) = rfd::FileDialog::new()
                    .add_filter("IMVU pose animation", &["xaf"])
                    .set_file_name("AvatarPose.xaf")
                    .save_file()
                else {
                    return Ok(None);
                };
                if path.extension().is_none() {
                    path.set_extension("xaf");
                }
                let xml = self.clip(self.active, true).export(&self.skeleton)?;
                atomic_write(&path, xml.as_bytes())?;
                Ok(Some(format!(
                    "Exported {}. Save project separately to keep editable work.",
                    path.display()
                )))
            }
        })();
        match result {
            Ok(Some(s)) => self.status = s,
            Ok(None) => {}
            Err(e) => self.status = format!("Could not export: {e}"),
        }
    }
    pub fn update(&mut self, ctx: &egui::Context) {
        if self.playing {
            self.time = (self.time + ctx.input(|i| i.stable_dt).min(0.1)) % self.scene.duration;
            self.sample_time();
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if !ctx.wants_keyboard_input() && self.pending.is_none() {
            if let Some(redo) = crate::shortcuts::undo_redo(ctx) {
                self.undo(redo)
            }
            if crate::shortcuts::pressed(ctx, crate::shortcuts::Command::Save) {
                self.save_project();
            }
            if crate::shortcuts::pressed(ctx, crate::shortcuts::Command::Frame) {
                self.camera = Camera {
                    zoom: 0.8,
                    ..Default::default()
                };
                self.redraw = true;
            }
        }
        if self.pending.is_none() && self.drag.is_none() {
            if let Some(action) = crate::shortcuts::consume(ctx) {
                match action {
                    crate::shortcuts::Action::Axis(axis) => {
                        if self.allowed(self.selected, axis) {
                            self.axis = axis;
                            self.status = format!(
                                "{} axis selected for {}.",
                                ["X", "Y", "Z"][axis],
                                joint_label(self.selected, &self.skeleton)
                            );
                        } else {
                            self.status=format!("{} rotation is locked for this joint. Enable Free joints to unlock it.",["X","Y","Z"][axis]);
                        }
                    }
                    crate::shortcuts::Action::Move => {
                        self.move_gizmo = true;
                        self.status="Move avatar: drag the arrows above the head. R returns to joint rotation.".into();
                    }
                    crate::shortcuts::Action::Rotate => {
                        self.move_gizmo = false;
                        self.status="Rotate joint: X / Y / Z select an allowed axis, then drag a joint or arrow.".into();
                    }
                }
                self.joints = true;
            }
        }
        egui::TopBottomPanel::top("poser_toolbar")
            .frame(crate::theme::toolbar_frame())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(crate::theme::title("AVATAR POSER"));
                    if ui.button("Open project…").clicked() {
                        self.request(Pending::Open)
                    }
                    if ui.add(crate::theme::primary("Save project…")).clicked() {
                        self.save_project();
                    }
                    if ui
                        .button(if self.scene.couple {
                            "Export couple XAFs…"
                        } else {
                            "Export XAF…"
                        })
                        .clicked()
                    {
                        self.export()
                    }
                    ui.separator();
                    for (name, yaw) in [
                        ("Front", 0.0),
                        ("Side", std::f32::consts::FRAC_PI_2),
                        ("Back", std::f32::consts::PI),
                    ] {
                        if ui.button(name).clicked() {
                            self.camera = Camera {
                                yaw,
                                zoom: 0.8,
                                ..Default::default()
                            };
                            self.redraw = true;
                        }
                    }
                    if let Some(redo) = crate::shortcuts::undo_redo_buttons(
                        ui,
                        !self.undo.is_empty(),
                        !self.redo.is_empty(),
                    ) {
                        self.undo(redo)
                    }
                });
            });
        egui::TopBottomPanel::bottom("poser_status")
            .frame(crate::theme::status_frame())
            .show(ctx, |ui| {
                ui.label(&self.status);
            });
        egui::SidePanel::left("poser_tools")
            .exact_width(306.0)
            .frame(crate::theme::inspector_frame())
            .show(ctx, |ui| {
                ui.add_enabled_ui(self.pending.is_none(), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.sidebar(ui));
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| self.viewport(ui));
        if let Some(p) = self.pending {
            egui::Window::new("Keep your pose work?")
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Save the current project before continuing?");
                    ui.horizontal(|ui| {
                        if ui.button("Save and continue").clicked() && self.save_project() {
                            self.pending = None;
                            self.perform(p)
                        }
                        if ui.button("Continue without saving").clicked() {
                            self.pending = None;
                            self.perform(p)
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending = None;
                        }
                    });
                });
        }
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        crate::theme::section(ui, "Avatar pose");
        ui.label("1 Click a joint / 2 Rotate / 3 Export XAF");
        let mut couple = self.scene.couple;
        if ui
            .checkbox(&mut couple, "Two avatars / couple pose")
            .changed()
        {
            self.checkpoint();
            self.scene.set_couple(couple);
            if !couple {
                self.active = 0;
            }
            self.redraw = true;
        }
        if self.scene.couple {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active, 0, "Avatar A · blue");
                ui.selectable_value(&mut self.active, 1, "Avatar B · rose");
            });
        }
        let mut variant = self.scene.actors[self.active].variant;
        ui.horizontal(|ui| {
            ui.label("Avatar:");
            ui.selectable_value(&mut variant, 0, "Female");
            ui.selectable_value(&mut variant, 1, "Male");
        });
        if variant != self.scene.actors[self.active].variant {
            self.checkpoint();
            self.scene.actors[self.active].variant = variant;
            self.redraw = true;
        }
        ui.horizontal(|ui| {
            if ui.button("Open XAF…").clicked() {
                self.request(Pending::Import)
            }
            if ui.button("Reset pose").clicked() {
                self.request(Pending::Reset)
            }
        });
        ui.add_space(8.0);
        crate::theme::group_title(ui, "SELECTED JOINT");
        ui.label(format!(
            "{} · {}",
            joint_label(self.selected, &self.skeleton),
            self.skeleton.bones[self.selected].name
        ));
        ui.horizontal(|ui| {
            for (a, label) in [(0, "X · red"), (1, "Y · green"), (2, "Z · blue")] {
                if ui
                    .add_enabled(
                        self.allowed(self.selected, a),
                        egui::Button::new(label).selected(self.axis == a),
                    )
                    .clicked()
                {
                    self.axis = a;
                }
            }
        });
        ui.label(
            egui::RichText::new({
                use crate::shortcuts::{text, Command};
                format!(
                    "{} / {} / {}: rotation axis · {}: avatar move gizmo · {}: joint rotation",
                    text(Command::AxisX),
                    text(Command::AxisY),
                    text(Command::AxisZ),
                    text(Command::Move),
                    text(Command::Rotate)
                )
            })
            .small()
            .weak(),
        );
        let old = self.scene.actors[self.active].angles[self.selected];
        let mut angles = old;
        let mut in_use = false;
        for index in 0..3 {
            let mut value = match index {
                0 => angles.0,
                1 => angles.1,
                _ => angles.2,
            };
            let r = ui.add_enabled(
                self.allowed(self.selected, index),
                egui::Slider::new(&mut value, -180.0..=180.0)
                    .text(rotation_label(self.selected, index, self.free_joints))
                    .suffix("°"),
            );
            if r.drag_started() {
                self.checkpoint();
                self.slider_editing = true;
            }
            if r.changed() {
                if !self.slider_editing {
                    self.checkpoint();
                }
                match index {
                    0 => angles.0 = value,
                    1 => angles.1 = value,
                    _ => angles.2 = value,
                }
                self.apply_angles(angles);
            }
            if r.drag_stopped() {
                self.slider_editing = false;
            }
            in_use |= r.dragged() || r.has_focus();
        }
        // Arrows are painted every frame, and releasing the slider is itself
        // an input event that repaints, so no extra redraw is needed.
        self.sliders_in_use = in_use;
        if ui.button("Reset this joint").clicked() {
            self.checkpoint();
            let rest = self.skeleton.rest_pose().local[self.selected];
            let actor = &mut self.scene.actors[self.active];
            actor.pose.local[self.selected] = rest;
            actor.reference.local[self.selected] = rest;
            actor.angles[self.selected] = V::default();
            self.autokey();
            self.redraw = true;
        }
        ui.collapsing("Move avatar / height", |ui| {
            let old = self.scene.actors[self.active].pose.local[1].position;
            let mut p = old;
            ui.label("Pelvis position (IMVU units). Changes move the full avatar.");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut p.0).speed(2.0).prefix("X "));
                ui.add(egui::DragValue::new(&mut p.1).speed(2.0).prefix("Y "));
                ui.add(egui::DragValue::new(&mut p.2).speed(2.0).prefix("Z "));
            });
            if p != old && [p.0, p.1, p.2].iter().all(|x| x.is_finite()) {
                self.checkpoint();
                self.scene.actors[self.active].pose.local[1].position = p;
                self.autokey();
                self.redraw = true;
            }
            ui.label(
                egui::RichText::new(
                    "Shift + left-drag a selected pelvis joint to move it in the view.",
                )
                .small()
                .weak(),
            );
        });
        ui.add_space(8.0);
        crate::theme::group_title(ui, "BODY PARTS");
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Find a joint…"));
        let filter = self.filter.to_lowercase();
        for (group, ids) in [
            ("Body + head", vec![1, 14, 15, 16, 17, 18, 19, 20, 21, 22]),
            ("Left arm", vec![24, 25, 26, 27, 28, 29]),
            ("Right arm", vec![55, 56, 57, 58, 59, 60]),
            ("Left leg", vec![2, 3, 4, 5, 6, 7]),
            ("Right leg", vec![8, 9, 10, 11, 12, 13]),
            ("Left fingers", (30..55).collect()),
            ("Right fingers", (61..86).collect()),
        ] {
            let ids: Vec<_> = ids
                .into_iter()
                .filter(|&i| {
                    (self.free_joints || native_joint_axes(i) != 0)
                        && (self.skeleton.bones[i].name.to_lowercase().contains(&filter)
                            || joint_label(i, &self.skeleton)
                                .to_lowercase()
                                .contains(&filter))
                })
                .collect();
            if ids.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(group)
                .default_open(!filter.is_empty())
                .show(ui, |ui| {
                    for i in ids {
                        if ui
                            .selectable_label(self.selected == i, joint_label(i, &self.skeleton))
                            .clicked()
                        {
                            self.select(self.active, i);
                            self.redraw = true;
                        }
                    }
                });
        }
        ui.separator();
        if ui
            .checkbox(&mut self.move_gizmo, "Move avatar gizmo")
            .on_hover_text(
                "World X/Y/Z arrows above the head. Orbit the view to reach a depth-facing arrow.",
            )
            .changed()
            && self.move_gizmo
        {
            self.joints = true;
        }
        if ui.checkbox(&mut self.all_bones, "Show all bones").changed() && self.all_bones {
            self.joints = true;
        }
        ui.checkbox(&mut self.joints, "Show joint controls");
        ui.checkbox(&mut self.fingers, "Show finger joints");
        if ui
            .checkbox(&mut self.free_joints, "Free joints (all rotation axes)")
            .changed()
        {
            self.select(self.active, self.selected);
        }
        ui.collapsing("Animation / keyframes (optional)", |ui| {
            ui.label(
                "Set a key at time 0. Move Time, pose a joint, and a new key is added \
                 automatically. Without keys, export holds your pose.",
            );
            let min_duration = self
                .scene
                .actors
                .iter()
                .flat_map(|a| a.keys.iter().map(|k| k.time))
                .fold(0.1_f32, f32::max);
            let mut duration = self.scene.duration;
            let field = egui::DragValue::new(&mut duration)
                .range(min_duration..=self.scene.duration.max(120.0))
                .speed(0.1)
                .suffix(" s duration");
            if ui.add(field).changed() {
                self.checkpoint();
                self.scene.duration = duration;
                self.time = self.time.min(duration);
            }
            let time = egui::Slider::new(&mut self.time, 0.0..=self.scene.duration)
                .text("Time")
                .suffix(" s");
            if ui.add(time).changed() {
                self.playing = false;
                self.sample_time();
            }
            ui.horizontal(|ui| {
                if ui
                    .button(if self.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    self.playing = !self.playing;
                }
                if ui.button("Set key").clicked() {
                    self.checkpoint();
                    self.set_key();
                }
                if ui.button("Delete key").clicked() {
                    self.checkpoint();
                    let now = self.time;
                    self.scene.actors[self.active]
                        .keys
                        .retain(|k| (k.time - now).abs() > 0.0001);
                }
            });
            let times: Vec<_> = self.scene.actors[self.active]
                .keys
                .iter()
                .map(|k| k.time)
                .collect();
            ui.horizontal_wrapped(|ui| {
                for t in times {
                    if ui.small_button(format!("{t:.2}s")).clicked() {
                        self.time = t;
                        self.sample_time();
                    }
                }
            });
        });
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Right-drag rotates the camera. Shift + right-drag pans. Wheel zooms. \
                 F frames the avatar. Save project keeps both avatars; export XAF is for IMVU.",
            )
            .small()
            .weak(),
        );
    }
    fn hit_actor(&self, point: [f32; 2]) -> Option<usize> {
        let mut hit = None;
        let mut nearest = f32::MAX;
        let mut offset = 0;
        for a in 0..if self.scene.couple { 2 } else { 1 } {
            let mesh = &self.meshes[self.scene.actors[a].variant];
            for face in &mesh.triangles {
                let Some(points) = face
                    .iter()
                    .map(|&i| self.projected.get(offset + i).copied())
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                if let Some(z) = triangle_depth(point, [points[0], points[1], points[2]]) {
                    if z < nearest {
                        nearest = z;
                        hit = Some(a);
                    }
                }
            }
            offset += mesh.positions.len();
        }
        hit
    }
    fn viewport(&mut self, ui: &mut egui::Ui) {
        let size = ui.available_size().max(Vec2::splat(10.0));
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let quality = if self.use_gpu {
            crate::gpu::logical_size(size)
        } else {
            self.preview_budget.choose(ui.ctx(), size, self.playing)
        };
        let [w, h] = quality.render;
        let resolution_changed = self.render_size != quality.render;
        if self.render_size[0] > 0 && resolution_changed {
            self.camera.pan[0] *= w as f32 / self.render_size[0] as f32;
            self.camera.pan[1] *= h as f32 / self.render_size[1] as f32;
        }
        self.render_size = quality.render;
        if self.redraw
            || resolution_changed
            || (!self.use_gpu
                && self
                    .preview
                    .as_ref()
                    .is_none_or(|t| t.size() != quality.display))
        {
            let actors: Vec<_> = self
                .scene
                .actors
                .iter()
                .take(if self.scene.couple { 2 } else { 1 })
                .map(|a| (&self.meshes[a.variant], &a.pose))
                .collect();
            if self.use_gpu {
                let scene = AvatarMesh::gpu_scene(&self.skeleton, &actors, &self.camera, [w, h]);
                self.projected = scene.projected.clone();
                self.gpu_scene = Some(std::sync::Arc::new(scene));
            } else {
                let frame =
                    AvatarMesh::render_actors(&self.skeleton, &actors, &self.camera, [w, h]);
                self.projected = frame.projected;
                let image = crate::render::antialias_preview(frame.image, quality.display);
                if let Some(t) = &mut self.preview {
                    t.set(image, egui::TextureOptions::LINEAR)
                } else {
                    self.preview = Some(ui.ctx().load_texture(
                        "avatar-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
            self.redraw = false;
        }
        let painter = ui.painter_at(rect);
        if self.use_gpu {
            if let (Some(gpu), Some(scene)) = (&self.gpu, &self.gpu_scene) {
                crate::gpu::paint(ui, rect, gpu.clone(), scene.clone());
            }
        } else if let Some(t) = &self.preview {
            painter.image(
                t.id(),
                rect,
                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let project = |p: V| {
            let p = self.camera.project(view_point(p), w, h);
            Pos2::new(
                rect.left() + p[0] * size.x / w as f32,
                rect.top() + p[1] * size.y / h as f32,
            )
        };
        let pointer = if response.drag_started_by(egui::PointerButton::Primary) {
            ui.input(|i| i.pointer.press_origin())
        } else {
            response
                .interact_pointer_pos()
                .or(ui.input(|i| i.pointer.hover_pos()))
        };
        let mut nearest = None;
        let mut distance = 12.0;
        let primary = [
            1, 3, 4, 5, 9, 10, 11, 14, 17, 21, 22, 25, 27, 28, 29, 56, 58, 59, 60,
        ];
        for actor in 0..if self.scene.couple { 2 } else { 1 } {
            let world = self.skeleton.world(&self.scene.actors[actor].pose);
            for bone in 1..86 {
                let is_finger = (30..55).contains(&bone) || (61..86).contains(&bone);
                if !self.all_bones
                    && ((!self.free_joints && native_joint_axes(bone) == 0)
                        || (!primary.contains(&bone)
                            && !(self.fingers && is_finger)
                            && !(actor == self.active && bone == self.selected)))
                {
                    continue;
                }
                let p = project(world[bone].position);
                if let Some(mouse) = pointer.filter(|_| self.joints) {
                    let d = mouse.distance(p);
                    if d < distance && rect.contains(mouse) {
                        nearest = Some((actor, bone));
                        distance = d;
                    }
                }
                if self.joints {
                    let selected = actor == self.active && bone == self.selected;
                    let color = if selected {
                        Color32::from_rgb(255, 218, 80)
                    } else if actor == 0 {
                        Color32::from_rgb(105, 212, 244)
                    } else {
                        Color32::from_rgb(246, 159, 184)
                    };
                    if let Some(parent) = self.skeleton.bones[bone].parent.filter(|p| *p != 0) {
                        painter.line_segment(
                            [project(world[parent].position), p],
                            Stroke::new(1.0_f32, color.gamma_multiply(0.45)),
                        );
                    }
                    painter.circle_filled(
                        p,
                        if selected { 6.5 } else { 3.5 },
                        Color32::from_black_alpha(190),
                    );
                    painter.circle_stroke(
                        p,
                        if selected { 7.0 } else { 4.0 },
                        Stroke::new(if selected { 2.5_f32 } else { 1.3_f32 }, color),
                    );
                    if selected {
                        painter.text(
                            p + Vec2::new(12.0, -14.0),
                            egui::Align2::LEFT_BOTTOM,
                            joint_label(bone, &self.skeleton),
                            egui::FontId::proportional(14.0),
                            color,
                        );
                    }
                }
            }
        }
        let mut move_hover = None;
        if self.move_gizmo && self.joints {
            let world = self.skeleton.world(&self.scene.actors[self.active].pose);
            let anchor = world[22].position.add(V(0.0, 0.0, 235.0));
            let origin = project(anchor);
            painter.circle_filled(origin, 4.0, Color32::WHITE);
            painter.text(
                origin + Vec2::new(-12.0, -15.0),
                egui::Align2::RIGHT_BOTTOM,
                "Move avatar",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
            for (direction, color, label) in [
                (V(1.0, 0.0, 0.0), Color32::from_rgb(255, 95, 95), "X"),
                (V(0.0, 1.0, 0.0), Color32::from_rgb(100, 235, 140), "Y"),
                (V(0.0, 0.0, 1.0), Color32::from_rgb(100, 170, 255), "Z"),
            ] {
                let end = project(anchor.add(direction.mul(110.0)));
                let vector = end - origin;
                if vector.length() < 10.0 {
                    continue;
                }
                painter.line_segment(
                    [origin, end],
                    Stroke::new(5.0_f32, Color32::from_black_alpha(190)),
                );
                painter.arrow(origin, vector, Stroke::new(2.5_f32, color));
                painter.text(
                    end + vector.normalized() * 10.0,
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(12.0),
                    color,
                );
                if pointer.is_some_and(|p| {
                    p.distance(origin) > 7.0
                        && crate::gizmo::segment_distance(p - origin, vector) < 9.0
                }) {
                    move_hover = Some((
                        world[0].rotation.conjugate().rotate(direction),
                        vector / 110.0,
                    ));
                }
            }
        }
        let mut axis_hover = None;
        if self.joints && !self.sliders_in_use {
            let world = self.skeleton.world(&self.scene.actors[self.active].pose);
            let joint = world[self.selected];
            let origin = project(joint.position);
            for (axis, direction, color, label) in [
                (0, V(1.0, 0.0, 0.0), Color32::from_rgb(255, 95, 95), "X"),
                (1, V(0.0, 1.0, 0.0), Color32::from_rgb(100, 235, 140), "Y"),
                (2, V(0.0, 0.0, 1.0), Color32::from_rgb(100, 170, 255), "Z"),
            ] {
                if !self.allowed(self.selected, axis) {
                    continue;
                }
                let end = project(
                    joint.position.add(
                        joint
                            .rotation
                            .rotate(if !self.free_joints && self.selected == 27 && axis == 2 {
                                direction.mul(-1.0)
                            } else {
                                direction
                            })
                            .mul(80.0),
                    ),
                );
                let delta = end - origin;
                if delta.length() < 8.0 {
                    continue;
                }
                painter.line_segment(
                    [origin, end],
                    Stroke::new(5.0_f32, Color32::from_black_alpha(190)),
                );
                painter.line_segment([origin, end], Stroke::new(2.5_f32, color));
                painter.circle_filled(end, 4.0, color);
                painter.text(
                    end + delta.normalized() * 12.0,
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(13.0),
                    color,
                );
                if pointer.is_some_and(|p| {
                    p.distance(origin) > 10.0
                        && crate::gizmo::segment_distance(p - origin, delta) < 8.0
                }) {
                    axis_hover = Some(axis);
                }
            }
        }
        // A different joint's dot lying under the selected joint's arrows (or
        // the move gizmo) must stay clickable: when the pointer is right on
        // that dot, the joint wins over the arrow. Elsewhere along the arrow,
        // clicks and drags still pick the axis.
        let on_other_joint = nearest.is_some_and(|(actor, bone)| {
            distance <= 8.0 && (actor, bone) != (self.active, self.selected)
        });
        if on_other_joint {
            axis_hover = None;
            move_hover = None;
        }
        // No hover ring while a drag is under way: the drag keeps the joint
        // and axis it started with, so passing over another dot must not
        // look like it will switch to it.
        if let Some((actor, bone)) = nearest.filter(|_| self.drag.is_none()) {
            let p = project(self.skeleton.world(&self.scene.actors[actor].pose)[bone].position);
            painter.circle_stroke(p, 10.0, Stroke::new(2.0_f32, Color32::WHITE));
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        if self.pending.is_some() {
            return;
        }
        if response.clicked_by(egui::PointerButton::Primary) {
            if move_hover.is_some() {
            } else if let Some(axis) = axis_hover {
                self.axis = axis;
            } else if let Some((a, b)) = nearest {
                self.select(a, b);
            } else {
                let hit = pointer.and_then(|p| {
                    self.hit_actor([
                        (p.x - rect.left()) * w as f32 / size.x,
                        (p.y - rect.top()) * h as f32 / size.y,
                    ])
                });
                self.joints = hit.is_some();
                if let Some(a) = hit {
                    self.active = a;
                }
            }
            ui.ctx().request_repaint();
        }
        if let Some((direction, per_unit)) =
            move_hover.filter(|_| response.drag_started_by(egui::PointerButton::Primary))
        {
            self.checkpoint();
            self.drag = Some(Drag::Translate {
                start: self.scene.actors[self.active].pose.local[1].position,
                direction,
                per_unit,
                delta: Vec2::ZERO,
            });
        }
        if response.drag_started_by(egui::PointerButton::Primary) && move_hover.is_none() {
            // The target is fixed where the press began (`pointer` is the press
            // origin here), so a drag that starts on an arrow keeps that axis
            // even when it later passes over another joint's dot.
            let target = if let Some(axis) = axis_hover {
                self.axis = axis;
                Some((self.active, self.selected))
            } else {
                nearest
            };
            if let Some((a, b)) = target {
                self.select(a, b);
                self.checkpoint();
                if b == 1 && ui.input(|i| i.modifiers.shift) {
                    self.drag = Some(Drag::Move {
                        start: self.scene.actors[a].pose.local[1].position,
                        delta: Vec2::ZERO,
                    });
                } else {
                    self.drag = Some(Drag::Rotate {
                        axis: self.axis,
                        start: self.scene.actors[a].angles[b],
                        delta: Vec2::ZERO,
                    });
                }
            }
        }
        if response.drag_started_by(egui::PointerButton::Secondary) {
            self.drag = Some(if ui.input(|i| i.modifiers.shift) {
                Drag::Pan
            } else {
                Drag::Orbit
            });
        }
        if response.drag_started_by(egui::PointerButton::Middle) {
            self.drag = Some(Drag::Pan);
        }
        if response.dragged() {
            let d = ui.input(|i| i.pointer.delta());
            let mut angle = None;
            let mut position = None;
            match &mut self.drag {
                Some(Drag::Translate {
                    start,
                    direction,
                    per_unit,
                    delta,
                }) => {
                    *delta += d;
                    position = Some(
                        start.add(direction.mul(crate::gizmo::axis_amount(*delta, *per_unit))),
                    );
                }
                Some(Drag::Orbit) => {
                    self.camera.yaw -= d.x * 0.009;
                    self.camera.pitch = (self.camera.pitch - d.y * 0.009).clamp(-1.45, 1.45);
                    self.redraw = true;
                }
                Some(Drag::Pan) => {
                    self.camera.pan[0] += d.x * w as f32 / size.x;
                    self.camera.pan[1] += d.y * h as f32 / size.y;
                    self.redraw = true;
                }
                Some(Drag::Rotate { axis, start, delta }) => {
                    *delta += d;
                    let mut a = *start;
                    let v = (delta.x - delta.y) * 0.5;
                    match *axis {
                        0 => a.0 = (a.0 + v).clamp(-180.0, 180.0),
                        1 => a.1 = (a.1 + v).clamp(-180.0, 180.0),
                        _ => a.2 = (a.2 + v).clamp(-180.0, 180.0),
                    }
                    angle = Some(a);
                }
                Some(Drag::Move { start, delta }) => {
                    *delta += d;
                    let depth = self.camera.project(view_point(*start), w, h)[2];
                    let movement = self
                        .camera
                        .screen_delta(
                            delta.x * w as f32 / size.x,
                            delta.y * h as f32 / size.y,
                            h as f32,
                            depth,
                        )
                        .mul(1.0 / 0.23);
                    position = Some(start.add(V(movement.0, movement.2, movement.1)));
                }
                None => {}
            }
            if let Some(a) = angle {
                self.apply_angles(a)
            }
            if let Some(p) = position {
                self.set_avatar_position(p);
            }
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom = (self.camera.zoom * (scroll * 0.002).exp()).clamp(0.2, 5.0);
                self.redraw = true;
            }
        }
        if self.redraw {
            ui.ctx().request_repaint();
        }
    }
}
fn triangle_depth(point: [f32; 2], p: [[f32; 3]; 3]) -> Option<f32> {
    if p.iter()
        .any(|p| p[2] < 10.0 || p.iter().any(|v| !v.is_finite()))
    {
        return None;
    }
    let edge = |a: [f32; 3], b: [f32; 3], q: [f32; 2]| {
        (q[0] - a[0]) * (b[1] - a[1]) - (q[1] - a[1]) * (b[0] - a[0])
    };
    let area = edge(p[0], p[1], [p[2][0], p[2][1]]);
    if area.abs() < 0.0001 {
        return None;
    }
    let a = edge(p[1], p[2], point) / area;
    let b = edge(p[2], p[0], point) / area;
    let c = 1.0 - a - b;
    if a < 0.0 || b < 0.0 || c < 0.0 {
        None
    } else {
        Some(1.0 / (a / p[0][2] + b / p[1][2] + c / p[2][2]))
    }
}
fn joint_label(i: usize, s: &Skeleton) -> String {
    match i {
        1 => "Pelvis / whole avatar".into(),
        3 => "Left hip".into(),
        4 => "Left knee".into(),
        5 => "Left ankle".into(),
        9 => "Right hip".into(),
        10 => "Right knee".into(),
        11 => "Right ankle".into(),
        14 => "Lower back".into(),
        17 => "Chest".into(),
        21 => "Neck".into(),
        22 => "Head".into(),
        25 => "Left shoulder".into(),
        27 => "Left elbow".into(),
        28 => "Left wrist".into(),
        29 => "Left hand".into(),
        56 => "Right shoulder".into(),
        58 => "Right elbow".into(),
        59 => "Right wrist".into(),
        60 => "Right hand".into(),
        _ => s.bones[i]
            .name
            .replace("lf", "Left ")
            .replace("rt", "Right "),
    }
}
fn read_bounded(p: &Path, max: u64) -> Result<Vec<u8>, String> {
    if std::fs::metadata(p).map_err(|e| e.to_string())?.len() > max {
        return Err("File exceeds the size limit".into());
    }
    std::fs::read(p).map_err(|e| e.to_string())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        std::fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
fn bake_root(s: &Skeleton, pose: &mut Pose) {
    let rest = s.rest_pose();
    let offset = rest.local[0].inverse().mul(pose.local[0]);
    pose.local[1] = offset.mul(pose.local[1]);
    pose.local[0] = rest.local[0];
}
fn static_clip(a: &Animation) -> bool {
    a.tracks.iter().all(|t| {
        let first = t.keys[0].transform;
        t.keys.iter().all(|k| {
            k.transform.position.sub(first.position).len() < 0.0001
                && k.transform.rotation.dot(first.rotation).abs() > 0.999999
        })
    })
}
fn load_scene(s: &Skeleton, bytes: &[u8]) -> Result<Scene, String> {
    let root = Element::parse(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    if root.name != "AVATARPOSE" || root.attributes.get("VERSION").map(String::as_str) != Some("1")
    {
        return Err("Not an Avatar Poser project version 1".into());
    }
    let actors: Vec<_> = root
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|e| e.name == "ACTOR")
        .collect();
    if actors.len() != 2 {
        return Err("Project must contain two actor slots".into());
    }
    let mut scene = Scene::new(s);
    scene.couple = root.attributes.get("COUPLE").map(String::as_str) == Some("1");
    scene.duration = 0.0;
    for (i, e) in actors.iter().enumerate() {
        let text = e.get_text().ok_or("Actor has no pose data")?;
        let a = Animation::load(s, text.as_bytes())?;
        let times: std::collections::BTreeSet<_> = a
            .tracks
            .iter()
            .flat_map(|t| t.keys.iter().map(|k| k.time.to_bits()))
            .collect();
        if times.len() > 500 {
            return Err("Project has too many keys".into());
        }
        let mut keys: Vec<_> = times
            .into_iter()
            .map(|t| {
                let time = f32::from_bits(t);
                Keyframe {
                    time,
                    pose: a.sample(s, time),
                }
            })
            .collect();
        keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        let mut pose = a.sample(s, 0.0);
        bake_root(s, &mut pose);
        for k in &mut keys {
            bake_root(s, &mut k.pose);
        }
        if e.attributes.get("MODE").map(String::as_str) == Some("held") {
            keys.clear();
        }
        scene.duration = scene.duration.max(a.duration);
        let variant = e
            .attributes
            .get("MODEL")
            .map(|v| v.parse::<usize>())
            .transpose()
            .map_err(|_| "Invalid avatar model")?
            .unwrap_or(i);
        if variant > 1 {
            return Err("Unknown avatar model".into());
        }
        let pair_offset = if let Some(v) = e.attributes.get("PAIR_OFFSET_X") {
            v.parse::<f32>().map_err(|_| "Invalid pair offset")?
        } else if scene.couple {
            pose.local[1].position.0 - s.bones[1].translation.0
        } else {
            0.0
        };
        if !pair_offset.is_finite() {
            return Err("Non-finite pair offset".into());
        }
        scene.pair_offsets[i] = if scene.couple { pair_offset } else { 0.0 };
        scene.actors[i] = Actor {
            reference: pose.clone(),
            angles: vec![V::default(); pose.local.len()],
            pose,
            keys,
            variant,
        };
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pose_key_sampling_interpolates_whole_pose() {
        let s = Skeleton::bundled();
        let a = s.rest_pose();
        let mut b = a.clone();
        b.local[1].position.0 += 100.0;
        let keys = vec![
            Keyframe {
                time: 0.0,
                pose: a.clone(),
            },
            Keyframe { time: 2.0, pose: b },
        ];
        let p = AvatarPoser::sample_keys(&keys, 1.0).unwrap();
        assert!((p.local[1].position.0 - a.local[1].position.0 - 50.0).abs() < 0.001);
    }
    #[test]
    fn invalid_project_is_rejected() {
        let s = Skeleton::bundled();
        assert!(load_scene(&s, b"<AVATARPOSE VERSION=\"1\"/>").is_err());
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;
    #[test]
    fn held_couple_projects_keep_both_offsets_without_creating_animation() {
        let mut p = AvatarPoser::new();
        p.scene.couple = true;
        p.scene.actors[0].pose.local[1].position.0 = -180.0;
        p.scene.actors[1].pose.local[1].position.0 = 180.0;
        let loaded = load_scene(&p.skeleton, &p.project_bytes().unwrap()).unwrap();
        assert!(loaded.couple);
        for (a, b) in loaded.actors.iter().zip(&p.scene.actors) {
            assert!(a.keys.is_empty());
            for (x, y) in a.pose.local.iter().zip(&b.pose.local) {
                assert!(x.position.sub(y.position).len() < 0.001);
                assert!(x.rotation.dot(y.rotation).abs() > 0.99999);
            }
        }
    }
    #[test]
    fn keyed_projects_preserve_animation_and_export_85_control_bones() {
        let mut p = AvatarPoser::new();
        p.set_key();
        p.time = 2.0;
        p.scene.actors[0].pose.local[27].rotation = p.scene.actors[0].pose.local[27]
            .rotation
            .mul(Quat::from_euler_degrees(V(0.0, 60.0, 0.0)));
        p.set_key();
        let clip = p.clip(0, true);
        assert_eq!(clip.tracks.len(), 85);
        let expected = clip.sample(&p.skeleton, 1.0);
        let loaded = load_scene(&p.skeleton, &p.project_bytes().unwrap()).unwrap();
        let actual = AvatarPoser::sample_keys(&loaded.actors[0].keys, 1.0).unwrap();
        for (a, b) in expected.local.iter().zip(actual.local) {
            assert!(a.position.sub(b.position).len() < 0.001);
            assert!(a.rotation.dot(b.rotation).abs() > 0.99999);
        }
    }
    #[test]
    fn source_joint_constraints_choose_elbow_and_knee_bend_axes() {
        let mut p = AvatarPoser::new();
        p.select(0, 27);
        assert_eq!(p.axis, 2);
        assert!(!p.allowed(27, 0));
        assert!(!p.allowed(27, 1));
        p.select(0, 4);
        assert_eq!(p.axis, 1);
        assert!(!p.allowed(4, 2));
        p.free_joints = true;
        assert!(p.allowed(4, 0));
    }
    #[test]
    fn baking_imported_scene_root_keeps_avatar_world_pose() {
        let s = Skeleton::bundled();
        let mut pose = s.rest_pose();
        pose.local[0].position.0 += 123.0;
        pose.local[0].rotation = pose.local[0]
            .rotation
            .mul(Quat::from_euler_degrees(V(0.0, 0.0, 30.0)));
        let before = s.world(&pose);
        bake_root(&s, &mut pose);
        let after = s.world(&pose);
        for i in 1..86 {
            assert!(before[i].position.sub(after[i].position).len() < 0.01);
            assert!(before[i].rotation.dot(after[i].rotation).abs() > 0.99999);
        }
    }
}

#[cfg(test)]
mod bend_regressions {
    use super::*;
    #[test]
    fn both_elbows_bend_forward_in_depth_instead_of_sideways() {
        let mut p = AvatarPoser::new();
        for (elbow, wrist) in [(27, 28), (58, 59)] {
            p.scene.actors[0] = Actor::new(&p.skeleton);
            p.select(0, elbow);
            assert_eq!(p.axis, 2);
            p.apply_angles(V(0.0, 0.0, 90.0));
            let world = p.skeleton.world(&p.scene.actors[0].pose);
            let forearm = world[wrist].position.sub(world[elbow].position);
            assert!(
                forearm.1 < -70.0,
                "elbow {elbow}: expected forward bend, got {forearm:?}"
            );
            assert!(
                forearm.0.abs() < 10.0,
                "elbow {elbow}: sideways bend {forearm:?}"
            );
            let clip = p.clip(0, true);
            let loaded =
                Animation::load(&p.skeleton, clip.export(&p.skeleton).unwrap().as_bytes()).unwrap();
            let exported = p.skeleton.world(&loaded.sample(&p.skeleton, 0.0));
            assert!(exported[wrist].position.sub(world[wrist].position).len() < 0.001);
        }
    }
    #[test]
    fn both_knees_bend_back_in_depth_instead_of_sideways() {
        let mut p = AvatarPoser::new();
        for (knee, ankle) in [(4, 5), (10, 11)] {
            p.scene.actors[0] = Actor::new(&p.skeleton);
            p.select(0, knee);
            assert_eq!(p.axis, 1);
            p.apply_angles(V(0.0, 90.0, 0.0));
            let w = p.skeleton.world(&p.scene.actors[0].pose);
            let shin = w[ankle].position.sub(w[knee].position);
            assert!(shin.1 > 240.0, "knee {knee}: wrong bend {shin:?}");
            assert!(shin.0.abs() < 15.0, "knee {knee}: sideways bend {shin:?}");
        }
    }
    #[test]
    fn flash_finger_and_toe_hinges_use_converted_native_axes() {
        for id in [
            6, 12, 32, 33, 37, 38, 42, 43, 47, 48, 52, 53, 63, 64, 68, 69, 73, 74, 78, 79, 83, 84,
        ] {
            assert_eq!(native_joint_axes(id), 2, "joint {id}");
        }
        assert_eq!(native_joint_axes(26), 1);
    }
}

#[cfg(test)]
mod couple_layout_regressions {
    use super::*;
    #[test]
    fn leaving_couple_restores_solo_position_for_pose_reference_and_all_keys() {
        let s = Skeleton::bundled();
        let mut scene = Scene::new(&s);
        let initial = scene.actors[0].pose.local[1].position.0;
        let mut later = scene.actors[0].pose.clone();
        later.local[1].position.0 += 50.0;
        scene.actors[0].keys = vec![
            Keyframe {
                time: 0.0,
                pose: scene.actors[0].pose.clone(),
            },
            Keyframe {
                time: 2.0,
                pose: later,
            },
        ];
        scene.set_couple(true);
        assert!((scene.actors[0].pose.local[1].position.0 + 180.0).abs() < 0.001);
        scene.set_couple(false);
        assert!((scene.actors[0].pose.local[1].position.0 - initial).abs() < 0.001);
        assert!((scene.actors[0].reference.local[1].position.0 - initial).abs() < 0.001);
        assert!((scene.actors[0].keys[0].pose.local[1].position.0 - initial).abs() < 0.001);
        assert!((scene.actors[0].keys[1].pose.local[1].position.0 - initial - 50.0).abs() < 0.001);
        for _ in 0..3 {
            scene.set_couple(true);
            scene.set_couple(false);
        }
        assert!((scene.actors[0].pose.local[1].position.0 - initial).abs() < 0.001);
    }
    #[test]
    fn pair_offsets_and_avatar_choices_survive_project_reload_and_undo() {
        let mut p = AvatarPoser::new();
        let original = p.scene.actors[0].pose.local[1].position.0;
        p.scene.actors[0].variant = 1;
        p.scene.actors[1].variant = 0;
        p.checkpoint();
        p.scene.set_couple(true);
        let mut loaded = load_scene(&p.skeleton, &p.project_bytes().unwrap()).unwrap();
        assert_eq!(loaded.actors[0].variant, 1);
        assert_eq!(loaded.actors[1].variant, 0);
        loaded.set_couple(false);
        assert!((loaded.actors[0].pose.local[1].position.0 - original).abs() < 0.001);
        p.undo(false);
        assert!(!p.scene.couple);
        assert!((p.scene.actors[0].pose.local[1].position.0 - original).abs() < 0.001);
        p.undo(true);
        assert!(p.scene.couple);
        p.scene.set_couple(false);
        assert!((p.scene.actors[0].pose.local[1].position.0 - original).abs() < 0.001);
    }
    #[test]
    fn manual_solo_translation_is_preserved_when_toggling_couple() {
        let s = Skeleton::bundled();
        let mut scene = Scene::new(&s);
        for a in &mut scene.actors {
            a.pose.local[1].position.0 = 75.0;
            a.reference = a.pose.clone();
        }
        scene.set_couple(true);
        scene.set_couple(false);
        assert!((scene.actors[0].pose.local[1].position.0 - 75.0).abs() < 0.001);
    }
}

#[cfg(test)]
mod legacy_pair_layout_test {
    use super::*;
    #[test]
    fn older_couple_project_without_offset_metadata_recenters_when_leaving() {
        let mut p = AvatarPoser::new();
        let original = p.scene.actors[0].pose.local[1].position.0;
        p.scene.set_couple(true);
        let mut root = Element::parse(Cursor::new(p.project_bytes().unwrap())).unwrap();
        for actor in root.children.iter_mut().filter_map(XMLNode::as_mut_element) {
            actor.attributes.remove("PAIR_OFFSET_X");
            actor.attributes.remove("MODEL");
        }
        let mut bytes = Vec::new();
        root.write(&mut bytes).unwrap();
        let mut scene = load_scene(&p.skeleton, &bytes).unwrap();
        scene.set_couple(false);
        assert!((scene.actors[0].pose.local[1].position.0 - original).abs() < 0.001);
    }
}

#[cfg(test)]
mod clean_preview_picking {
    use super::*;
    #[test]
    fn floor_and_background_do_not_count_as_clicking_an_avatar() {
        let mut p = AvatarPoser::new();
        let f = AvatarMesh::render_actors(
            &p.skeleton,
            &[(&p.meshes[0], &p.scene.actors[0].pose)],
            &p.camera,
            [320, 320],
        );
        let floor = [160.0, 305.0];
        assert!(
            f.depth[305 * 320 + 160] < f32::MAX,
            "point should hit visible floor"
        );
        p.projected = f.projected;
        assert_eq!(p.hit_actor(floor), None);
        assert_eq!(p.hit_actor([4.0, 4.0]), None);
        let world = p.skeleton.world(&p.scene.actors[0].pose);
        let v = p.camera.project(view_point(world[17].position), 320, 320);
        assert_eq!(p.hit_actor([v[0], v[1]]), Some(0));
    }
}

#[cfg(test)]
mod avatar_move_regression {
    use super::*;
    #[test]
    fn whole_avatar_movement_keeps_joint_rotations_and_exports_with_undo() {
        let mut p = AvatarPoser::new();
        p.scene.set_couple(true);
        p.active = 1;
        let before = p.scene.actors[1].pose.clone();
        let untouched = p.scene.actors[0].pose.clone();
        let delta = V(42.0, -35.0, 21.0);
        p.set_key();
        p.checkpoint();
        p.set_avatar_position(before.local[1].position.add(delta));
        let old = p.skeleton.world(&before);
        let current = p.skeleton.world(&p.scene.actors[1].pose);
        for i in 1..86 {
            assert!(current[i].position.sub(old[i].position).sub(delta).len() < 0.01);
            assert_eq!(
                before.local[i].rotation,
                p.scene.actors[1].pose.local[i].rotation
            );
        }
        assert_eq!(p.scene.actors[0].pose, untouched);
        assert_eq!(
            p.scene.actors[1].keys[0].pose.local[1].position,
            p.scene.actors[1].pose.local[1].position
        );
        let clip = p.clip(1, true);
        let xml = clip.export(&p.skeleton).unwrap();
        let loaded = Animation::load(&p.skeleton, xml.as_bytes())
            .unwrap()
            .sample(&p.skeleton, 0.0);
        assert!(
            loaded.local[1]
                .position
                .sub(before.local[1].position)
                .sub(delta)
                .len()
                < 0.01
        );
        p.undo(false);
        assert_eq!(p.scene.actors[1].pose, before);
    }
}
