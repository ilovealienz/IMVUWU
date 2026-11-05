//! Packages workspace: look up an IMVU product, discover its revisions,
//! download a revision or open a local ZIP/CHKN/folder, then inspect and
//! save the original files.
//!
//! Network work runs on background threads and reports through a channel.
//! Each request carries an ID; results for an older request (a product
//! looked up before the current one, a cancelled scan) are ignored. Nothing
//! here touches the editor workspaces.

use crate::{
    imvu::{self, Failure, ProductInfo, RevisionState, ScanEvent, ScanPlan},
    package::{self, Kind, Package, Source},
    theme,
};
use eframe::egui::{self, Color32, Vec2};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, atomic::Ordering, mpsc, Arc},
};

enum Message {
    Product {
        request: u64,
        result: Result<ProductInfo, Failure>,
    },
    Thumbnail {
        request: u64,
        image: Option<egui::ColorImage>,
    },
    /// The parent product's name, looked up after the product itself.
    ParentName {
        request: u64,
        result: Result<Option<String>, Failure>,
    },
    Scan {
        request: u64,
        event: ScanEvent,
    },
    Progress {
        key: (u64, u32),
        done: usize,
        total: usize,
    },
    Downloaded {
        key: (u64, u32),
        result: Result<Package, Failure>,
    },
    /// A gallery thumbnail for file `file` of the package with `package` ID;
    /// `None` when the image could not be decoded.
    Thumb {
        package: u64,
        file: usize,
        image: Option<egui::ColorImage>,
    },
}

struct ProductView {
    id: u64,
    request: u64,
    state: Result<Option<ProductInfo>, Failure>,
    thumbnail: Option<egui::TextureHandle>,
    /// `None` while still looking it up (or when there is no parent).
    parent_name: Option<Result<Option<String>, Failure>>,
}

struct ScanView {
    product: u64,
    request: u64,
    plan: ScanPlan,
    cancel: Arc<AtomicBool>,
    results: BTreeMap<u32, RevisionState>,
    finished: Option<(u32, bool)>,
}

struct DownloadView {
    done: usize,
    total: usize,
    cancel: Arc<AtomicBool>,
}

struct Preview {
    key: (usize, usize, u64),
    content: PreviewContent,
}

enum PreviewContent {
    Image {
        texture: egui::TextureHandle,
        size: [usize; 2],
        translucent: bool,
    },
    Text(String),
    Note(String),
    /// A turnable preview of an XMF/CMF mesh.
    Mesh {
        /// The mesh as stored, with its morph targets.
        source: Arc<crate::mesh::StaticMesh>,
        /// `source` posed as chosen below: what is drawn and exported.
        mesh: Arc<crate::mesh::StaticMesh>,
        /// Apply Face.Average, as IMVU does (only offered when the mesh has it).
        face_average: bool,
        /// Expressions (.xpf/.cpf) in the package: name and morph weights.
        expressions: Vec<(String, Vec<(String, f32)>)>,
        expression: Option<usize>,
        images: Arc<BTreeMap<usize, image::RgbaImage>>,
        textured_materials: usize,
        yaw: f32,
        pitch: f32,
        texture: Option<egui::TextureHandle>,
        rendered: Option<(f32, f32, [usize; 2])>,
    },
}

/// Expressions (.xpf/.cpf) in a package that load, each as its morph weights
/// at its strongest key time (the pose a held expression shows).
fn package_expressions(package: &Package) -> Vec<(String, Vec<(String, f32)>)> {
    package
        .files
        .iter()
        .filter(|f| {
            let n = f.name.to_ascii_lowercase();
            n.ends_with(".xpf") || n.ends_with(".cpf")
        })
        .filter_map(|f| {
            let animation = crate::xpf::Animation::load(f.bytes.as_ref().ok()?).ok()?;
            let weights_at = |time: f32| -> Vec<(String, f32)> {
                animation
                    .tracks
                    .iter()
                    .map(|t| (t.name.clone(), animation.weight(&t.name, time)))
                    .collect()
            };
            let strength = |w: &[(String, f32)]| w.iter().map(|(_, v)| v.abs()).sum::<f32>();
            let best = animation
                .tracks
                .iter()
                .flat_map(|t| t.keys.iter().map(|k| k.time))
                .map(weights_at)
                .max_by(|a, b| strength(a).total_cmp(&strength(b)))?;
            Some((f.name.clone(), best))
        })
        .collect()
}

/// `source` with Face.Average and the chosen expression applied.
fn pose(
    source: &crate::mesh::StaticMesh,
    face_average: bool,
    expressions: &[(String, Vec<(String, f32)>)],
    expression: Option<usize>,
) -> crate::mesh::StaticMesh {
    let weights = expression
        .and_then(|i| expressions.get(i))
        .map_or(&[][..], |(_, w)| &w[..]);
    source.posed(face_average, weights)
}

/// Is this file a Cal3D mesh (binary CMF or XMF text)?
/// Columns the file list can be sorted by (click a heading).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortBy {
    Name,
    Type,
    Size,
}

/// What the details pane's header buttons asked for.
enum HeaderAction {
    /// Save the file: `None` keeps the stored name, `Some` uses that one.
    Save(Option<String>),
    ExportObj(crate::mesh::ObjMode),
}

/// The Type column's text for a file (also its sort key).
fn type_text(file: &package::PackageFile) -> String {
    match (&file.kind, &file.bytes) {
        (_, Err(e)) => e.clone(),
        (Some(k), _) => k.label(),
        (None, _) => String::new(),
    }
}

/// File indices in display order for `sort`.
fn sorted_files(files: &[package::PackageFile], sort: Option<(SortBy, bool)>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    let Some((by, descending)) = sort else {
        return order;
    };
    let name = |i: usize| files[i].name.to_lowercase();
    order.sort_by(|&a, &b| {
        let primary = match by {
            SortBy::Name => name(a).cmp(&name(b)),
            SortBy::Type => type_text(&files[a])
                .to_lowercase()
                .cmp(&type_text(&files[b]).to_lowercase()),
            SortBy::Size => files[a].size().cmp(&files[b].size()),
        };
        let primary = if descending {
            primary.reverse()
        } else {
            primary
        };
        primary.then_with(|| name(a).cmp(&name(b)))
    });
    order
}

fn is_mesh(kind: &Kind, bytes: &[u8]) -> bool {
    match kind {
        Kind::Cal3d(label, _) => label.starts_with("CMF"),
        Kind::Xml => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_string();
            head.contains("MAGIC=\"XMF\"") || head.contains("<MESH")
        }
        _ => false,
    }
}

pub struct Browser {
    client: imvu::Client,
    sender: mpsc::Sender<Message>,
    receiver: mpsc::Receiver<Message>,
    next_request: u64,
    query: String,
    product: Option<ProductView>,
    scan: Option<ScanView>,
    scan_last: u32,
    revision_input: String,
    downloads: BTreeMap<(u64, u32), DownloadView>,
    packages: Vec<Package>,
    /// Stable ID per open package (parallel to `packages`), so thumbnails
    /// stay attached when another package is closed.
    package_ids: Vec<u64>,
    /// Gallery thumbnails by (package ID, file index); `None` = not an image
    /// this build can decode.
    thumbs: BTreeMap<(u64, usize), Option<egui::TextureHandle>>,
    /// Bumped whenever `packages` changes, so previews are rebuilt.
    packages_version: u64,
    /// References of the shown package, keyed by (package ID, version).
    references: Option<((u64, u64), Vec<package::Reference>)>,
    selected: Option<usize>,
    selected_file: Option<usize>,
    /// File list order: a column and descending?, or the package's order.
    sort: Option<(SortBy, bool)>,
    preview: Option<Preview>,
    status: String,
    /// Set by `--packages <id>`; looked up on the first frame.
    pub initial_lookup: Option<u64>,
    /// Set by `--packages <path>`; opened on the first frame.
    pub initial_open: Option<std::path::PathBuf>,
    /// Set by `--select <name>`: the file to select in that package.
    pub initial_select: Option<String>,
}

impl Default for Browser {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            client: imvu::Client::default(),
            sender,
            receiver,
            next_request: 1,
            query: String::new(),
            product: None,
            scan: None,
            scan_last: ScanPlan::default().last,
            revision_input: String::new(),
            downloads: BTreeMap::new(),
            packages: Vec::new(),
            package_ids: Vec::new(),
            thumbs: BTreeMap::new(),
            packages_version: 0,
            references: None,
            selected: None,
            selected_file: None,
            sort: None,
            preview: None,
            status: "Enter a product ID or shop link, or open a local ZIP, CHKN or folder.".into(),
            initial_lookup: None,
            initial_open: None,
            initial_select: None,
        }
    }
}

/// Only fetch thumbnails from IMVU's own hosts.
fn trusted_image_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.contains('@') && (host == "imvu.com" || host.ends_with(".imvu.com"))
}

/// Decodes by content (signature), falling back to TGA, which has no
/// signature, so a misnamed TGA still previews. Bounded to 8192×8192.
fn decode_image(name: &str, bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let decode = |format: Option<image::ImageFormat>| -> Result<image::RgbaImage, String> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
        match format {
            Some(f) => reader.set_format(f),
            None => reader = reader.with_guessed_format().map_err(|e| e.to_string())?,
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        reader.limits(limits);
        Ok(reader.decode().map_err(|e| e.to_string())?.into_rgba8())
    };
    // Signature first even for `.tga` names: a PNG named .tga is a PNG.
    let _ = name;
    decode(None).or_else(|first| decode(Some(image::ImageFormat::Tga)).map_err(|_| first))
}

fn color_image(image: &image::RgbaImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    )
}

/// Links to IMVU's own pages for a product (opened in the web browser):
/// shop page, derivation tree, products derived from it (when derivation is
/// allowed), the creator's shop and the raw API record.
fn product_links(ui: &mut egui::Ui, info: &ProductInfo) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let link = |ui: &mut egui::Ui, text: &str, url: String, tip: &str| {
            ui.hyperlink_to(egui::RichText::new(text).small(), url)
                .on_hover_text(tip);
        };
        link(
            ui,
            "Shop page",
            info.shop_url(),
            "The product's page in the IMVU shop",
        );
        link(
            ui,
            "Derivation tree",
            info.derivation_tree_url(),
            "Products this one derives from, and products derived from it",
        );
        if info.allows_derivation != Some(false) {
            link(
                ui,
                "Derived products",
                info.derived_products_url(),
                "Shop search for products made from this one",
            );
        }
        if let Some(url) = info.creator_shop_url() {
            link(ui, "Creator's shop", url, "Everything this creator sells");
        }
        link(
            ui,
            "API",
            info.api_url(),
            "The raw product record from api.imvu.com",
        );
    });
}

/// Short display form of a stored name: `%XX` escapes decoded.
fn display_name(name: &str) -> String {
    package::percent_decode(name)
}

/// What index.xml and the materials refer to, and what is wrong with it.
/// Returns true when "Save with original names restored…" was clicked.
fn reference_summary(ui: &mut egui::Ui, references: &[package::Reference]) -> bool {
    use package::RefState;
    if references.is_empty() {
        return false;
    }
    let present = references
        .iter()
        .filter(|r| r.state == RefState::Present)
        .count();
    let not_here: Vec<&package::Reference> = references
        .iter()
        .filter(|r| r.state == RefState::NotHere)
        .collect();
    let renamed: Vec<&package::Reference> = references
        .iter()
        .filter(|r| {
            matches!(
                r.state,
                RefState::OtherExtension(_) | RefState::AddedExtension(_)
            )
        })
        .collect();
    ui.label(
        egui::RichText::new(format!(
            "File references in index.xml and materials: {present} of {} found in this package.",
            references.len()
        ))
        .small(),
    );
    if !not_here.is_empty() {
        let mut names: Vec<String> = not_here.iter().map(|r| display_name(&r.target)).collect();
        names.dedup();
        let shown = names.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
        ui.label(
            egui::RichText::new(format!(
                "Not in this package (inherited from the parent, or missing): {shown}{}",
                if names.len() > 6 {
                    format!(" and {} more", names.len() - 6)
                } else {
                    String::new()
                }
            ))
            .small()
            .color(theme::ORANGE),
        );
    }
    let mut restore = false;
    if !renamed.is_empty() {
        ui.label(
            egui::RichText::new(format!(
                "{} reference(s) point to names that are only here under a different \
                 extension, so IMVU and other tools will not find them. This copy was \
                 probably renamed by a downloader, for example:",
                renamed.len()
            ))
            .small()
            .color(theme::ORANGE),
        );
        for r in renamed.iter().take(4) {
            let current = match &r.state {
                RefState::OtherExtension(c) | RefState::AddedExtension(c) => c,
                _ => continue,
            };
            ui.label(
                egui::RichText::new(format!(
                    "   {} wants “{}”, found “{}”",
                    display_name(&r.from),
                    display_name(&r.target),
                    display_name(current)
                ))
                .small()
                .monospace(),
            );
        }
        restore = ui
            .small_button("Save with original names restored…")
            .on_hover_text(
                "Saves a .chkn copy whose files use the exact names index.xml and the materials \
                 refer to. File contents are unchanged.",
            )
            .clicked();
    }
    restore
}

fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

impl Browser {
    fn request_id(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }

    fn look_up(&mut self, id: u64, ctx: &egui::Context) {
        if let Some(scan) = &self.scan {
            scan.cancel.store(true, Ordering::Relaxed);
        }
        self.scan = None;
        let request = self.request_id();
        self.product = Some(ProductView {
            id,
            request,
            state: Ok(None),
            thumbnail: None,
            parent_name: None,
        });
        self.query = id.to_string();
        let (client, sender, repaint) = (self.client.clone(), self.sender.clone(), ctx.clone());
        std::thread::spawn(move || {
            let ctx = repaint;
            let result = client.product(id);
            let image_url = result.as_ref().ok().and_then(|p| p.image_url.clone());
            let parent = result.as_ref().ok().and_then(|p| p.parent);
            let _ = sender.send(Message::Product { request, result });
            ctx.request_repaint();
            if let Some(parent) = parent {
                let result = client.product(parent).map(|p| p.name);
                let _ = sender.send(Message::ParentName { request, result });
                ctx.request_repaint();
            }
            if let Some(url) = image_url.filter(|u| trusted_image_url(u)) {
                let image = client
                    .get(&url, 4 * 1024 * 1024)
                    .ok()
                    .and_then(|b| decode_image(&url, &b).ok())
                    .map(|i| {
                        let i =
                            image::imageops::thumbnail(&i, 96.min(i.width()), 96.min(i.height()));
                        color_image(&i)
                    });
                let _ = sender.send(Message::Thumbnail { request, image });
                ctx.request_repaint();
            }
        });
        self.start_scan(id, ctx);
    }

    fn start_scan(&mut self, id: u64, ctx: &egui::Context) {
        if let Some(scan) = &self.scan {
            scan.cancel.store(true, Ordering::Relaxed);
        }
        let request = self.request_id();
        let plan = ScanPlan {
            last: self.scan_last.max(1),
            ..ScanPlan::default()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.scan = Some(ScanView {
            product: id,
            request,
            plan,
            cancel: cancel.clone(),
            results: BTreeMap::new(),
            finished: None,
        });
        let (client, sender, ctx) = (self.client.clone(), self.sender.clone(), ctx.clone());
        std::thread::spawn(move || {
            let (events, received) = mpsc::channel();
            let forward = std::thread::spawn(move || {
                for event in received {
                    if sender.send(Message::Scan { request, event }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            imvu::scan_revisions(client, id, plan, cancel, events);
            let _ = forward.join();
        });
    }

    fn download(&mut self, product: u64, revision: u32, ctx: &egui::Context) {
        let key = (product, revision);
        if self.downloads.contains_key(&key) {
            return;
        }
        if let Some(index) = self
            .packages
            .iter()
            .position(|p| p.source == Source::Remote { product, revision })
        {
            self.select(Some(index));
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.downloads.insert(
            key,
            DownloadView {
                done: 0,
                total: 0,
                cancel: cancel.clone(),
            },
        );
        self.status = format!("Downloading {product} revision {revision}…");
        let (client, sender, ctx) = (self.client.clone(), self.sender.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = client.contents(product, revision).map(|entries| {
                let total = entries.len();
                let _ = sender.send(Message::Progress {
                    key,
                    done: 0,
                    total,
                });
                let progress = |done: usize| {
                    let _ = sender.send(Message::Progress { key, done, total });
                    ctx.request_repaint();
                };
                let files = imvu::download_revision(
                    &client, product, revision, &entries, &cancel, &progress,
                );
                package::from_download(product, revision, files)
            });
            let _ = sender.send(Message::Downloaded { key, result });
            ctx.request_repaint();
        });
    }

    fn add_package(&mut self, package: Package, ctx: &egui::Context) {
        let id = self.request_id();
        // Gallery thumbnails are decoded off the UI thread.
        let images: Vec<(usize, String, Arc<Vec<u8>>)> = package
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.kind.as_ref().is_some_and(Kind::can_preview))
            .filter_map(|(i, f)| Some((i, f.name.clone(), f.bytes.as_ref().ok()?.clone())))
            .collect();
        if !images.is_empty() {
            let (sender, ctx) = (self.sender.clone(), ctx.clone());
            std::thread::spawn(move || {
                for (file, name, bytes) in images {
                    let image = decode_image(&name, &bytes).ok().map(|i| {
                        let scale = 128.0 / i.width().max(i.height()) as f32;
                        let thumb = if scale < 1.0 {
                            image::imageops::thumbnail(
                                &i,
                                ((i.width() as f32 * scale) as u32).max(1),
                                ((i.height() as f32 * scale) as u32).max(1),
                            )
                        } else {
                            i
                        };
                        color_image(&thumb)
                    });
                    if sender
                        .send(Message::Thumb {
                            package: id,
                            file,
                            image,
                        })
                        .is_err()
                    {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        }
        self.packages.push(package);
        self.package_ids.push(id);
        self.packages_version += 1;
        self.select(Some(self.packages.len() - 1));
    }

    fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        self.selected_file = index.and_then(|i| {
            let files = &self.packages[i].files;
            files
                .iter()
                .position(|f| f.name.eq_ignore_ascii_case("index.xml"))
                .or((!files.is_empty()).then_some(0))
        });
        self.preview = None;
    }

    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                Message::Product { request, result } => {
                    if let Some(view) = self.product.as_mut().filter(|v| v.request == request) {
                        view.state = result.map(Some);
                    }
                }
                Message::Thumbnail { request, image } => {
                    if let Some(view) = self.product.as_mut().filter(|v| v.request == request) {
                        view.thumbnail = image.map(|i| {
                            ctx.load_texture("product-thumbnail", i, egui::TextureOptions::LINEAR)
                        });
                    }
                }
                Message::ParentName { request, result } => {
                    if let Some(view) = self.product.as_mut().filter(|v| v.request == request) {
                        view.parent_name = Some(result);
                    }
                }
                Message::Scan { request, event } => {
                    if let Some(scan) = self.scan.as_mut().filter(|s| s.request == request) {
                        match event {
                            ScanEvent::Checked(revision, state) => {
                                scan.results.insert(revision, state);
                            }
                            ScanEvent::Finished {
                                last_checked,
                                stopped_early,
                            } => scan.finished = Some((last_checked, stopped_early)),
                        }
                    }
                }
                Message::Progress { key, done, total } => {
                    if let Some(d) = self.downloads.get_mut(&key) {
                        d.done = done;
                        d.total = total;
                    }
                }
                Message::Downloaded { key, result } => {
                    let cancelled = self
                        .downloads
                        .remove(&key)
                        .is_some_and(|d| d.cancel.load(Ordering::Relaxed));
                    match result {
                        Ok(package) if cancelled => {
                            self.status = format!(
                                "Cancelled {} revision {}; nothing was kept.",
                                key.0, key.1
                            );
                            drop(package);
                        }
                        Ok(package) => {
                            self.status = if package.is_complete() {
                                format!(
                                    "Downloaded {} revision {}: {} files.",
                                    key.0,
                                    key.1,
                                    package.files.len()
                                )
                            } else {
                                format!(
                                    "Downloaded {} revision {} with problems; see the notes.",
                                    key.0, key.1
                                )
                            };
                            self.add_package(package, ctx);
                        }
                        Err(e) => {
                            self.status =
                                format!("Could not read {} revision {}: {e}", key.0, key.1);
                        }
                    }
                }
                Message::Thumb {
                    package,
                    file,
                    image,
                } => {
                    // Ignore thumbnails for a package closed in the meantime.
                    if self.package_ids.contains(&package) {
                        let texture = image.map(|i| {
                            ctx.load_texture(
                                format!("thumb-{package}-{file}"),
                                i,
                                egui::TextureOptions::LINEAR,
                            )
                        });
                        self.thumbs.insert((package, file), texture);
                    }
                }
            }
        }
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.initial_lookup.take() {
            self.look_up(id, ctx);
        }
        if let Some(path) = self.initial_open.take() {
            let opened = if path.is_dir() {
                package::open_folder(&path)
            } else {
                package::open_archive(&path)
            };
            match opened {
                Ok(p) => self.add_package(p, ctx),
                Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
            }
            if let (Some(name), Some(i)) = (self.initial_select.take(), self.selected) {
                self.selected_file = self.packages[i]
                    .files
                    .iter()
                    .position(|f| f.name.eq_ignore_ascii_case(&name));
                self.preview = None;
            }
        }
        self.receive(ctx);
        egui::TopBottomPanel::top("browser_toolbar")
            .frame(theme::toolbar_frame())
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(theme::title("PACKAGES"));
                    if ui.button("Open ZIP / CHKN…").clicked() {
                        self.open_archive_dialog(ctx);
                    }
                    if ui.button("Open folder…").clicked() {
                        self.open_folder_dialog(ctx);
                    }
                });
            });
        egui::TopBottomPanel::bottom("browser_status")
            .frame(theme::status_frame())
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&self.status).small());
            });
        egui::SidePanel::left("browser_tools")
            .resizable(true)
            .default_width(300.0)
            .width_range(260.0..=460.0)
            .frame(theme::inspector_frame())
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.sidebar(ui, ctx));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::BG_DARK).inner_margin(10))
            .show(ctx, |ui| self.package_view(ui, ctx));
    }

    fn open_archive_dialog(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("IMVU package", &["zip", "chkn"])
            .pick_file()
        {
            match package::open_archive(&path) {
                Ok(p) => {
                    self.status = format!("Opened {} ({} files).", path.display(), p.files.len());
                    self.add_package(p, ctx);
                }
                Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
            }
        }
    }

    fn open_folder_dialog(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            match package::open_folder(&path) {
                Ok(p) => {
                    self.status = format!("Opened {} ({} files).", path.display(), p.files.len());
                    self.add_package(p, ctx);
                }
                Err(e) => self.status = format!("Could not open {}: {e}", path.display()),
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        theme::section(ui, "Find product");
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Product ID or shop link")
                    // Leave room for the button and spacing, or the row is
                    // wider than the panel and the panel grows every frame.
                    .desired_width((ui.available_width() - 90.0).max(80.0)),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add(theme::primary("Look up")).clicked() || entered {
                match imvu::parse_product_input(&self.query) {
                    Some(id) => self.look_up(id, ctx),
                    None => self.status = "That is not a product ID or IMVU product link.".into(),
                }
            }
        });
        let mut look_up_parent = None;
        if let Some(view) = &self.product {
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if let Some(t) = &view.thumbnail {
                        let size = t.size_vec2();
                        let scale = (72.0 / size.x.max(size.y)).min(1.0);
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                            t.id(),
                            size * scale,
                        )));
                    }
                    ui.vertical(|ui| match &view.state {
                        Ok(None) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(format!("Looking up {}…", view.id));
                            });
                        }
                        Ok(Some(info)) => {
                            ui.label(
                                egui::RichText::new(
                                    info.name.as_deref().unwrap_or("(no name given)"),
                                )
                                .strong()
                                .color(theme::FG_BRIGHT),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "#{} · by {}",
                                    info.id,
                                    info.creator.as_deref().unwrap_or("unknown")
                                ))
                                .small(),
                            );
                            let kind = info.type_summary();
                            if !kind.is_empty() {
                                ui.label(egui::RichText::new(kind).small().color(theme::ACCENT))
                                    .on_hover_text("Product type, as the IMVU API states it");
                            }
                            let mut facts = Vec::new();
                            if let Some(r) = &info.rating {
                                facts.push(r.clone());
                            }
                            if let Some(p) = &info.price {
                                facts.push(format!("{p} credits"));
                            }
                            if let Some(d) = info.allows_derivation {
                                facts.push(if d { "derivable" } else { "not derivable" }.into());
                            }
                            if !facts.is_empty() {
                                ui.label(egui::RichText::new(facts.join(" · ")).small().weak());
                            }
                            if !info.categories.is_empty() {
                                ui.label(
                                    egui::RichText::new(info.categories.join(" › "))
                                        .small()
                                        .weak(),
                                );
                            }
                            if let Some(parent) = info.parent {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(egui::RichText::new("Derives from").small().weak());
                                    let label = match &view.parent_name {
                                        None => format!("#{parent} …"),
                                        Some(Ok(Some(name))) => format!("{name} (#{parent})"),
                                        Some(Ok(None)) => format!("#{parent}"),
                                        Some(Err(e)) => format!("#{parent} ({e})"),
                                    };
                                    if ui
                                        .small_button(label)
                                        .on_hover_text(
                                            "Look up the parent product and its revisions",
                                        )
                                        .clicked()
                                    {
                                        look_up_parent = Some(parent);
                                    }
                                });
                            }
                            product_links(ui, info);
                        }
                        Err(e) => {
                            ui.label(egui::RichText::new(format!("#{}", view.id)).strong());
                            ui.label(
                                egui::RichText::new(format!("Details {e}."))
                                    .small()
                                    .color(theme::ORANGE),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Its files may still be available; see revisions below.",
                                )
                                .small()
                                .weak(),
                            );
                            // The shop pages can still work when the API says 401.
                            product_links(
                                ui,
                                &ProductInfo {
                                    id: view.id,
                                    ..Default::default()
                                },
                            );
                        }
                    });
                });
            });
        }
        if let Some(parent) = look_up_parent {
            self.look_up(parent, ctx);
        }

        ui.add_space(6.0);
        theme::group_title(ui, "REVISIONS");
        let product = self.product.as_ref().map(|p| p.id);
        ui.horizontal(|ui| {
            ui.label("Check 1 to");
            ui.add(egui::DragValue::new(&mut self.scan_last).range(1..=5000));
            let scanning = self.scan.as_ref().is_some_and(|s| s.finished.is_none());
            if scanning {
                if ui.button("Stop").clicked() {
                    if let Some(s) = &self.scan {
                        s.cancel.store(true, Ordering::Relaxed);
                    }
                }
            } else if ui
                .add_enabled(product.is_some(), egui::Button::new("Scan"))
                .clicked()
            {
                if let Some(id) = product {
                    self.start_scan(id, ctx);
                }
            }
        });
        let mut open_revision = None;
        if let Some(scan) = &self.scan {
            let found: Vec<u32> = scan
                .results
                .iter()
                .filter(|(_, s)| **s == RevisionState::Exists)
                .map(|(r, _)| *r)
                .collect();
            let unknown: Vec<(u32, &Failure)> = scan
                .results
                .iter()
                .filter_map(|(r, s)| match s {
                    RevisionState::Unknown(f) => Some((*r, f)),
                    _ => None,
                })
                .collect();
            let summary = match scan.finished {
                None => format!(
                    "Checking… {} of up to {} checked",
                    scan.results.len(),
                    scan.plan.last
                ),
                Some((last, stopped)) => format!(
                    "Checked {}–{}{}. Revisions beyond that, or after a long gap, may exist.",
                    scan.plan.first,
                    last,
                    if stopped { " (stopped)" } else { "" }
                ),
            };
            ui.label(egui::RichText::new(summary).small().weak());
            if found.is_empty() && scan.finished.is_some() {
                ui.label(
                    egui::RichText::new("No revisions found in that range.")
                        .small()
                        .color(theme::ORANGE),
                );
            }
            ui.horizontal_wrapped(|ui| {
                for revision in found {
                    let key = (scan.product, revision);
                    let busy = self.downloads.contains_key(&key);
                    let open = self.packages.iter().any(|p| {
                        p.source
                            == Source::Remote {
                                product: key.0,
                                revision,
                            }
                    });
                    let label = if busy {
                        format!("rev {revision} …")
                    } else if open {
                        format!("rev {revision} ✔")
                    } else {
                        format!("rev {revision}")
                    };
                    if ui
                        .add_enabled(!busy, egui::Button::new(label))
                        .on_hover_text("Download and open this revision's files")
                        .clicked()
                    {
                        open_revision = Some(key);
                    }
                }
            });
            if !unknown.is_empty() {
                let list: Vec<String> = unknown
                    .iter()
                    .take(8)
                    .map(|(r, f)| format!("{r} ({f})"))
                    .collect();
                ui.label(
                    egui::RichText::new(format!(
                        "Could not check: {}{}",
                        list.join(", "),
                        if unknown.len() > 8 { "…" } else { "" }
                    ))
                    .small()
                    .color(theme::ORANGE),
                );
            }
        }
        ui.horizontal(|ui| {
            ui.label("Open revision");
            ui.add(
                egui::TextEdit::singleline(&mut self.revision_input)
                    .desired_width(60.0)
                    .hint_text("#"),
            );
            let revision = self
                .revision_input
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|&r| r > 0);
            if ui
                .add_enabled(
                    product.is_some() && revision.is_some(),
                    egui::Button::new("Open"),
                )
                .clicked()
            {
                open_revision = product.zip(revision);
            }
        });
        if let Some((p, r)) = open_revision {
            self.download(p, r, ctx);
        }
        for ((p, r), d) in &self.downloads {
            ui.horizontal(|ui| {
                let fraction = if d.total == 0 {
                    0.0
                } else {
                    d.done as f32 / d.total as f32
                };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(ui.available_width() - 60.0)
                        .text(format!("{p} rev {r}: {}/{}", d.done, d.total)),
                );
                if ui.small_button("Cancel").clicked() {
                    d.cancel.store(true, Ordering::Relaxed);
                }
            });
        }

        ui.add_space(6.0);
        theme::group_title(ui, "OPEN PACKAGES");
        if self.packages.is_empty() {
            ui.label(egui::RichText::new("None yet.").small().weak());
        }
        let mut close = None;
        let mut choose = None;
        for (i, p) in self.packages.iter().enumerate() {
            ui.horizontal(|ui| {
                let label = if p.is_complete() {
                    p.title.clone()
                } else {
                    format!("{} (partial)", p.title)
                };
                if ui
                    .selectable_label(self.selected == Some(i), label)
                    .clicked()
                {
                    choose = Some(i);
                }
                if ui.small_button("×").on_hover_text("Close").clicked() {
                    close = Some(i);
                }
            });
        }
        if let Some(i) = choose {
            self.select(Some(i));
        }
        if let Some(i) = close {
            self.packages.remove(i);
            let id = self.package_ids.remove(i);
            self.thumbs.retain(|(package, _), _| *package != id);
            self.packages_version += 1;
            let next = match self.selected {
                Some(s) if s == i => None,
                Some(s) if s > i => Some(s - 1),
                s => s,
            };
            self.select(next);
        }
    }

    fn package_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(index) = self.selected.filter(|&i| i < self.packages.len()) else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(
                        "Look up a product and pick a revision, or open a ZIP, CHKN or folder.",
                    )
                    .color(theme::MUTED),
                );
            });
            return;
        };
        let mut look_up = None;
        let mut restore = false;
        // Reference checks parse every material, so they are cached per
        // package until the package list changes.
        let cache_key = (self.package_ids[index], self.packages_version);
        if self
            .references
            .as_ref()
            .is_none_or(|(key, _)| *key != cache_key)
        {
            self.references = Some((cache_key, self.packages[index].references()));
        }
        {
            let references = &self.references.as_ref().unwrap().1;
            let package = &self.packages[index];
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&package.title)
                        .size(16.0)
                        .strong()
                        .color(theme::FG_BRIGHT),
                );
                let source = match &package.source {
                    Source::Archive(p) | Source::Folder(p) => p.display().to_string(),
                    Source::Remote { product, revision } => {
                        format!("IMVU product data, product {product}, revision {revision}")
                    }
                };
                ui.label(egui::RichText::new(source).small().weak());
            });
            for note in &package.notes {
                ui.label(egui::RichText::new(note).small().color(theme::ORANGE));
            }
            match package.index() {
                None => {
                    ui.label(
                        egui::RichText::new(
                            "No index.xml: this is not a complete product package.",
                        )
                        .small()
                        .weak(),
                    );
                }
                Some(Err(e)) => {
                    ui.label(
                        egui::RichText::new(format!("index.xml could not be read: {e}"))
                            .small()
                            .color(theme::ORANGE),
                    );
                }
                Some(Ok(summary)) => {
                    theme::card().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if summary.imports.is_empty() {
                            ui.label(
                                egui::RichText::new("Inherits nothing: index.xml has no import.")
                                    .small(),
                            );
                        }
                        for id in &summary.imports {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Inherits from product {id}. Settings and files not in this \
                                         package come from it and are not included here."
                                    ))
                                    .small(),
                                );
                                if ui.small_button("Look up").clicked() {
                                    look_up = Some(*id);
                                }
                            });
                        }
                        if !summary.deletions.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Removes from the parent: {}",
                                    summary
                                        .deletions
                                        .iter()
                                        .map(|d| d.trim_start_matches("Template/"))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ))
                                .small(),
                            );
                        }
                        let _ = &summary;
                        if reference_summary(ui, references) {
                            restore = true;
                        }
                    });
                }
            }
        }
        if let Some(id) = look_up {
            self.look_up(id, ctx);
        }
        if restore {
            self.save_restored(index);
        }
        ui.horizontal(|ui| {
            let file = self
                .selected_file
                .and_then(|f| self.packages[index].files.get(f))
                .filter(|f| f.bytes.is_ok());
            if ui
                .add_enabled(file.is_some(), egui::Button::new("Save file…"))
                .clicked()
            {
                self.save_selected_file(index, None);
            }
            if ui.button("Save all to folder…").clicked() {
                self.save_all(index);
            }
            if ui.button("Save as .chkn…").clicked() {
                self.save_archive(index, "chkn");
            }
            if ui.button("Save as .zip…").clicked() {
                self.save_archive(index, "zip");
            }
            let images = self.packages[index]
                .files
                .iter()
                .any(|f| f.kind.as_ref().is_some_and(Kind::is_image));
            if ui
                .add_enabled(images, egui::Button::new("Save images, fixed names…"))
                .on_hover_text(
                    "A separate copy of just the images, each named with the extension that \
                     matches its contents, plus renamed.txt. For use outside IMVU: index.xml \
                     still uses the original names.",
                )
                .clicked()
            {
                self.save_images(index);
            }
        });
        ui.separator();
        let list_width = (ui.available_width() * 0.45).clamp(260.0, 520.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(list_width);
                egui::ScrollArea::vertical()
                    .id_salt("package_files")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.gallery(ui, index);
                        self.file_list(ui, index);
                    });
            });
            ui.separator();
            ui.vertical(|ui| self.preview_pane(ui, ctx, index));
        });
    }

    /// Thumbnails of every image in the package; click to preview.
    fn gallery(&mut self, ui: &mut egui::Ui, index: usize) {
        let id = self.package_ids[index];
        let images: Vec<(usize, &package::PackageFile)> = self.packages[index]
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.bytes.is_ok() && f.kind.as_ref().is_some_and(Kind::is_image))
            .collect();
        if images.is_empty() {
            return;
        }
        let mut choose = None;
        egui::CollapsingHeader::new(format!("Images ({})", images.len()))
            .id_salt(("gallery", id))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(6.0);
                    for (i, file) in images {
                        let size = Vec2::splat(64.0);
                        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 0.0, theme::BG_ALT);
                        match self.thumbs.get(&(id, i)) {
                            Some(Some(texture)) => {
                                let t = texture.size_vec2();
                                let fit = t * (size.x / t.x.max(t.y));
                                let image_rect = egui::Rect::from_center_size(rect.center(), fit);
                                painter.image(
                                    texture.id(),
                                    image_rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    Color32::WHITE,
                                );
                            }
                            Some(None) => {
                                painter.text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "no preview",
                                    egui::FontId::proportional(10.0),
                                    theme::MUTED,
                                );
                            }
                            None if file.kind.as_ref().is_some_and(Kind::can_preview) => {
                                painter.text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "…",
                                    egui::FontId::proportional(14.0),
                                    theme::MUTED,
                                );
                            }
                            None => {
                                painter.text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    file.kind.as_ref().map(Kind::label).unwrap_or_default(),
                                    egui::FontId::proportional(10.0),
                                    theme::MUTED,
                                );
                            }
                        }
                        let selected = self.selected_file == Some(i);
                        let mismatch = file.extension_mismatch().is_some();
                        let stroke = if selected {
                            egui::Stroke::new(2.0_f32, theme::ACCENT)
                        } else if response.hovered() {
                            egui::Stroke::new(1.0_f32, theme::ACCENT)
                        } else {
                            egui::Stroke::new(1.0_f32, theme::SURFACE)
                        };
                        painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
                        // Informational marker: IMVU itself often stores
                        // images under a name for another format.
                        if mismatch {
                            painter.circle_filled(
                                rect.right_top() + Vec2::new(-7.0, 7.0),
                                3.5,
                                theme::MUTED,
                            );
                        }
                        let tip = match file.extension_mismatch() {
                            Some(ext) if ext.is_empty() => format!(
                                "{}\nStored as {}; the name has no extension",
                                file.name,
                                file.kind.as_ref().map(Kind::label).unwrap_or_default()
                            ),
                            Some(ext) => format!(
                                "{}\nStored as {} under a .{ext} name",
                                file.name,
                                file.kind.as_ref().map(Kind::label).unwrap_or_default()
                            ),
                            None => file.name.clone(),
                        };
                        if response.on_hover_text(tip).clicked() {
                            choose = Some(i);
                        }
                    }
                });
            });
        if let Some(i) = choose {
            self.selected_file = Some(i);
            self.preview = None;
        }
    }

    fn file_list(&mut self, ui: &mut egui::Ui, index: usize) {
        let mut choose = None;
        let mut new_sort = None;
        egui::Grid::new("files_grid")
            .striped(true)
            .num_columns(3)
            .spacing(Vec2::new(10.0, 3.0))
            .show(ui, |ui| {
                for (by, header) in [
                    (SortBy::Name, "Name"),
                    (SortBy::Type, "Type"),
                    (SortBy::Size, "Size"),
                ] {
                    let active = self.sort.filter(|(b, _)| *b == by);
                    let text = match active {
                        Some((_, false)) => format!("{header} \u{23f6}"),
                        Some((_, true)) => format!("{header} \u{23f7}"),
                        None => header.to_string(),
                    };
                    let label =
                        egui::RichText::new(text)
                            .small()
                            .strong()
                            .color(if active.is_some() {
                                theme::ACCENT
                            } else {
                                theme::MUTED
                            });
                    let response = ui
                        .add(egui::Label::new(label).sense(egui::Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Click to sort; click again to reverse");
                    if response.clicked() {
                        new_sort = Some(match active {
                            Some((_, descending)) => (by, !descending),
                            None => (by, by == SortBy::Size),
                        });
                    }
                }
                ui.end_row();
                let files = &self.packages[index].files;
                for i in sorted_files(files, self.sort) {
                    let file = &files[i];
                    let mut name = egui::RichText::new(&file.name);
                    if file.bytes.is_err() {
                        name = name.color(theme::RED);
                    }
                    if ui
                        .selectable_label(self.selected_file == Some(i), name)
                        .clicked()
                    {
                        choose = Some(i);
                    }
                    let kind = type_text(file);
                    // Note images whose name says another format. This is
                    // normal on IMVU (base avatar .tga textures are JPEGs),
                    // so it is information, not a warning.
                    match file.extension_mismatch() {
                        Some(ext) => {
                            let named = if ext.is_empty() {
                                "no extension".to_string()
                            } else {
                                format!("named .{ext}")
                            };
                            ui.label(
                                egui::RichText::new(format!("{kind} · {named}"))
                                    .small()
                                    .color(theme::MUTED),
                            )
                            .on_hover_text(
                                "The contents are a different format from the name. IMVU often \
                                 stores images this way and reads them by content, so the file \
                                 is kept as it is. “Save images, fixed names” makes a copy with \
                                 matching extensions for other programs.",
                            );
                        }
                        None => {
                            ui.label(egui::RichText::new(kind).small());
                        }
                    }
                    ui.label(
                        egui::RichText::new(file.size().map(human_size).unwrap_or_default())
                            .small(),
                    );
                    ui.end_row();
                }
            });
        if let Some(i) = choose {
            self.selected_file = Some(i);
            self.preview = None;
        }
        if new_sort.is_some() {
            self.sort = new_sort;
        }
    }

    fn preview_pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, index: usize) {
        let Some(file_index) = self.selected_file else {
            return;
        };
        match self.file_header(ui, index, file_index) {
            Some(HeaderAction::Save(name)) => self.save_selected_file(index, name),
            Some(HeaderAction::ExportObj(mode)) => self.export_obj(index, file_index, mode),
            None => {}
        }
        let Some(file) = self.packages[index].files.get(file_index) else {
            return;
        };
        let mut facts = Vec::new();
        if let Some(size) = file.size() {
            facts.push(format!("{size} bytes"));
        }
        if !file.tags.is_empty() {
            facts.push(format!("tags: {}", file.tags.join(", ")));
        }
        if let Some(d) = &file.listed_dimensions {
            facts.push(format!("listed size {}", d.replace(',', "×")));
        }
        if let Some(t) = &file.server_type {
            facts.push(format!("server sent {t}"));
        }
        ui.label(egui::RichText::new(facts.join(" · ")).small().weak());
        if let (Some(ext), Some(kind)) = (file.extension_mismatch(), &file.kind) {
            let named = if ext.is_empty() {
                "has no extension".to_string()
            } else {
                format!("is named .{ext}")
            };
            ui.label(
                egui::RichText::new(format!(
                    "Stored as a {} but the file {named}. That is common on IMVU, which \
                     reads images by content, so saves keep this name. Other programs may \
                     need “Save images, fixed names”, which names it {}.",
                    kind.label(),
                    file.corrected_name().unwrap_or_default()
                ))
                .small()
                .color(theme::MUTED),
            );
        }
        let key = (index, file_index, self.packages_version);
        if self.preview.as_ref().is_none_or(|p| p.key != key) {
            let content = match (&file.bytes, &file.kind) {
                (Err(e), _) => PreviewContent::Note(format!("Not available: {e}")),
                (Ok(_), Some(Kind::Webp)) => PreviewContent::Note(
                    "WebP image. This build has no WebP decoder, so there is no preview; \
                     the file itself is kept and saved unchanged."
                        .into(),
                ),
                (Ok(bytes), Some(kind)) if kind.is_image() => match decode_image(&file.name, bytes)
                {
                    Ok(image) => PreviewContent::Image {
                        size: [image.width() as usize, image.height() as usize],
                        translucent: image.pixels().any(|p| p[3] < 255),
                        texture: ctx.load_texture(
                            "package-preview",
                            color_image(&image),
                            egui::TextureOptions::LINEAR,
                        ),
                    },
                    Err(e) => PreviewContent::Note(format!("Could not decode image: {e}")),
                },
                (Ok(bytes), Some(kind)) if is_mesh(kind, bytes) => {
                    match crate::mesh::StaticMesh::parse(bytes) {
                        Ok(mesh) => {
                            let package = &self.packages[index];
                            let images: BTreeMap<usize, image::RgbaImage> =
                                crate::mesh::diffuse_textures(package)
                                    .into_iter()
                                    .filter_map(|(id, name)| {
                                        let bytes = package.file(&name)?.bytes.as_ref().ok()?;
                                        Some((id, crate::settings::decode_texture(bytes).ok()?))
                                    })
                                    .collect();
                            let textured_materials = mesh
                                .submeshes
                                .iter()
                                .filter(|s| images.contains_key(&s.material))
                                .count();
                            let expressions = package_expressions(package);
                            let expression = (!expressions.is_empty()).then_some(0);
                            let source = Arc::new(mesh);
                            let mesh = Arc::new(pose(&source, true, &expressions, expression));
                            PreviewContent::Mesh {
                                source,
                                mesh,
                                face_average: true,
                                expressions,
                                expression,
                                images: Arc::new(images),
                                textured_materials,
                                yaw: 0.0,
                                pitch: 0.15,
                                texture: None,
                                rendered: None,
                            }
                        }
                        Err(e) => PreviewContent::Note(format!("Could not read this mesh: {e}")),
                    }
                }
                (Ok(bytes), Some(Kind::Xml | Kind::Text)) => {
                    let shown = &bytes[..bytes.len().min(256 * 1024)];
                    let mut text = String::from_utf8_lossy(shown).into_owned();
                    if shown.len() < bytes.len() {
                        text.push_str("\n… (first 256 KB shown)");
                    }
                    PreviewContent::Text(text)
                }
                (Ok(bytes), Some(Kind::Cal3d(..))) if package::material_maps(bytes).is_some() => {
                    // A material: list its texture maps and whether each is here.
                    let package = &self.packages[index];
                    let text = match package::material_maps(bytes) {
                        Some(Ok(maps)) if maps.is_empty() => {
                            "Material with no texture maps.".into()
                        }
                        Some(Ok(maps)) => {
                            let lines: Vec<String> = maps
                                .iter()
                                .map(|m| {
                                    let state = match package.state_of(&m.file) {
                                        package::RefState::Present => "found".to_string(),
                                        package::RefState::NotHere => {
                                            "not in this package (inherited or missing)".into()
                                        }
                                        package::RefState::OtherExtension(c)
                                        | package::RefState::AddedExtension(c) => format!(
                                            "only here as “{}” (renamed?)",
                                            display_name(&c)
                                        ),
                                    };
                                    format!(
                                        "{}: {}\n    {state}",
                                        if m.kind.is_empty() { "Map" } else { &m.kind },
                                        m.file
                                    )
                                })
                                .collect();
                            format!("Material texture maps\n\n{}", lines.join("\n"))
                        }
                        Some(Err(e)) => format!("Material could not be read: {e}"),
                        None => String::new(),
                    };
                    PreviewContent::Text(text)
                }
                (Ok(bytes), kind) => {
                    let hex: Vec<String> =
                        bytes.iter().take(64).map(|b| format!("{b:02x}")).collect();
                    PreviewContent::Note(format!(
                        "{}. No preview for this type yet.\nFirst bytes: {}",
                        kind.as_ref().map(Kind::label).unwrap_or_default(),
                        hex.join(" ")
                    ))
                }
            };
            self.preview = Some(Preview { key, content });
        }
        let Some(preview) = &self.preview else { return };
        match &preview.content {
            PreviewContent::Image {
                texture,
                size,
                translucent,
            } => {
                ui.label(
                    egui::RichText::new(format!(
                        "{}×{} pixels{}",
                        size[0],
                        size[1],
                        if *translucent {
                            " · has transparency (shown over a checkerboard)"
                        } else {
                            ""
                        }
                    ))
                    .small(),
                );
                let available = ui.available_size();
                let image_size = Vec2::new(size[0] as f32, size[1] as f32);
                let scale = (available.x / image_size.x)
                    .min(available.y / image_size.y)
                    .min(4.0);
                let shown = image_size * scale;
                let (rect, _) = ui.allocate_exact_size(shown, egui::Sense::hover());
                // Checkerboard behind the image; the saved file is untouched.
                let cell = 10.0;
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 0.0, theme::BG_ALT);
                let (cols, rows) = (
                    (rect.width() / cell).ceil() as i32,
                    (rect.height() / cell).ceil() as i32,
                );
                for y in 0..rows {
                    for x in 0..cols {
                        if (x + y) % 2 == 0 {
                            let min = rect.min + Vec2::new(x as f32 * cell, y as f32 * cell);
                            painter.rect_filled(
                                egui::Rect::from_min_size(min, Vec2::splat(cell)),
                                0.0,
                                theme::SURFACE,
                            );
                        }
                    }
                }
                painter.image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            PreviewContent::Text(text) => {
                // Wrapped: many IMVU index.xml files are a single long line.
                egui::ScrollArea::vertical()
                    .id_salt("package_text")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).monospace().size(12.0))
                                .wrap_mode(egui::TextWrapMode::Wrap)
                                .selectable(true),
                        );
                    });
            }
            PreviewContent::Note(note) => {
                ui.label(egui::RichText::new(note).small());
            }
            PreviewContent::Mesh { .. } => {}
        }
        self.mesh_preview(ui, ctx);
    }

    /// Draws the turnable mesh preview (its OBJ exports are in the header).
    fn mesh_preview(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(Preview {
            content:
                PreviewContent::Mesh {
                    source,
                    mesh,
                    face_average,
                    expressions,
                    expression,
                    images,
                    textured_materials,
                    yaw,
                    pitch,
                    texture,
                    rendered,
                },
            ..
        }) = &mut self.preview
        else {
            return;
        };
        let has_face_average = source.has_morph(crate::mesh::FACE_AVERAGE);
        if has_face_average || !expressions.is_empty() {
            let mut changed = false;
            ui.horizontal_wrapped(|ui| {
                if has_face_average {
                    changed |= ui
                        .checkbox(face_average, "Face.Average")
                        .on_hover_text(
                            "Draw the head with its Face.Average morph, as IMVU does. \
                             Heads made from a template keep the creator's face shape \
                             only in this morph; off shows the bare base mesh.",
                        )
                        .changed();
                }
                if !expressions.is_empty() {
                    ui.label("Expression");
                    let label = |e: Option<usize>| {
                        e.and_then(|i| expressions.get(i))
                            .map_or("None".to_string(), |(name, _)| display_name(name).to_string())
                    };
                    egui::ComboBox::from_id_salt("mesh-expression")
                        .selected_text(label(*expression))
                        .show_ui(ui, |ui| {
                            for choice in std::iter::once(None).chain((0..expressions.len()).map(Some)) {
                                changed |= ui
                                    .selectable_value(expression, choice, label(choice))
                                    .changed();
                            }
                        })
                        .response
                        .on_hover_text(
                            "Apply an expression from this package to the head's morphs, \
                             at its strongest point",
                        );
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} morph(s) · drawn and exported as shown",
                        source.morph_count()
                    ))
                    .small()
                    .color(theme::MUTED),
                );
            });
            if changed {
                *mesh = Arc::new(pose(source, *face_average, expressions, *expression));
                *rendered = None;
            }
        }
        let (low, high) = mesh.bounds();
        let unused = mesh.unused_vertices();
        ui.label(
            egui::RichText::new(format!(
                "{} vertices · {} faces · {} part(s), {} textured · size {:.0} × {:.0} × {:.0} \
                 (native units){} · drag to turn",
                mesh.vertex_count(),
                mesh.face_count(),
                mesh.submeshes.len(),
                textured_materials,
                high.0 - low.0,
                high.1 - low.1,
                high.2 - low.2,
                if unused > 0 {
                    format!(" · {unused} unused point(s) not drawn, kept in exports")
                } else {
                    String::new()
                },
            ))
            .small(),
        );
        let available = ui.available_size();
        let side = available.x.min(available.y).clamp(120.0, 640.0);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), egui::Sense::drag());
        if response.dragged() {
            let delta = response.drag_delta();
            *yaw += delta.x * 0.01;
            *pitch = (*pitch + delta.y * 0.01).clamp(-1.4, 1.4);
        }
        let pixels = ((side * ctx.pixels_per_point()) as usize).clamp(64, 900);
        let size = [pixels, pixels];
        if *rendered != Some((*yaw, *pitch, size)) {
            let image = crate::mesh::render_preview(mesh, images, *yaw, *pitch, size);
            match texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => {
                    *texture =
                        Some(ctx.load_texture("mesh-preview", image, egui::TextureOptions::LINEAR))
                }
            }
            *rendered = Some((*yaw, *pitch, size));
        }
        if let Some(t) = texture {
            ui.painter().image(
                t.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    /// Writes `<stem>.obj`, `<stem>.mtl` and the textures it uses into a new
    /// folder; never overwrites.
    fn export_obj(&mut self, index: usize, file_index: usize, mode: crate::mesh::ObjMode) {
        let package = &self.packages[index];
        let Some(file) = package.files.get(file_index) else {
            return;
        };
        let Ok(bytes) = &file.bytes else { return };
        // The previewed pose when this file is the one shown, else as stored.
        let shown = match &self.preview {
            Some(Preview {
                key,
                content: PreviewContent::Mesh { mesh, .. },
            }) if key.0 == index && key.1 == file_index => Some(mesh.clone()),
            _ => None,
        };
        let mesh = match shown {
            Some(m) => m,
            None => match crate::mesh::StaticMesh::parse(bytes) {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    self.status = format!("Could not export: {e}");
                    return;
                }
            },
        };
        let Some(parent) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let stem = package::stem(&file.name).to_string();
        let blender = mode == crate::mesh::ObjMode::Blender;
        let folder = parent.join(format!(
            "{stem}_{}",
            if blender {
                "obj_blender"
            } else {
                "obj_unmodified"
            }
        ));
        // Texture names: as stored (unmodified) or with the extension that
        // matches the contents (Blender-ready).
        let mut textures = BTreeMap::new();
        let mut copies = Vec::new();
        for (id, name) in crate::mesh::diffuse_textures(package) {
            let Some(texture) = package.file(&name) else {
                continue;
            };
            let Ok(data) = &texture.bytes else { continue };
            let out_name = if blender {
                texture
                    .corrected_name()
                    .unwrap_or_else(|| texture.name.clone())
            } else {
                texture.name.clone()
            };
            let out_name = out_name.rsplit('/').next().unwrap_or(&out_name).to_string();
            if !copies.iter().any(|(n, _): &(String, _)| *n == out_name) {
                copies.push((out_name.clone(), data.clone()));
            }
            textures.insert(id, out_name);
        }
        let (obj, mtl) = crate::mesh::to_obj(&mesh, &stem, mode, &textures);
        let result = (|| -> Result<(), String> {
            if folder.exists() {
                return Err(format!("{} already exists", folder.display()));
            }
            std::fs::create_dir(&folder).map_err(|e| e.to_string())?;
            package::write_new_file(&folder.join(format!("{stem}.obj")), obj.as_bytes())?;
            if let Some(mtl) = &mtl {
                package::write_new_file(&folder.join(format!("{stem}.mtl")), mtl.as_bytes())?;
            }
            for (name, data) in &copies {
                if crate::imvu::is_safe_name(name) {
                    package::write_new_file(&folder.join(name), data)?;
                }
            }
            for (name, text) in crate::mesh::blend_launcher(&stem, mode) {
                package::write_new_file(&folder.join(name), text.as_bytes())?;
                #[cfg(unix)]
                if name.ends_with(".sh") {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        folder.join(name),
                        std::fs::Permissions::from_mode(0o755),
                    );
                }
            }
            Ok(())
        })();
        self.status = match result {
            Ok(()) => format!(
                "Exported {} to {} ({} texture(s)){}",
                file.name,
                folder.display(),
                copies.len(),
                if blender {
                    "; run \"Make blend.cmd\" (Windows) or \"Make blend.sh\" there to open it in Blender."
                } else {
                    "; native units, Z-up. \"Make blend.cmd\" / \"Make blend.sh\" there opens it in Blender."
                }
            ),
            Err(e) => format!("Nothing exported: {e}"),
        };
    }

    /// The details pane title: the file name, with Download buttons on the
    /// right. Returns the clicked choice: `None` keeps the stored name,
    /// `Some(name)` saves under the name that matches the contents.
    fn file_header(
        &self,
        ui: &mut egui::Ui,
        index: usize,
        file_index: usize,
    ) -> Option<HeaderAction> {
        let file = self.packages[index].files.get(file_index)?;
        let available = file.bytes.is_ok();
        let mesh = match (&file.kind, &file.bytes) {
            (Some(kind), Ok(bytes)) => is_mesh(kind, bytes),
            _ => false,
        };
        let fixed = file
            .extension_mismatch()
            .and(file.corrected_name())
            .map(|n| n.rsplit('/').next().unwrap_or(&n).to_string());
        let mut choice = None;
        // One Download button whose menu lists every way to save this file,
        // with the file name below it.
        ui.horizontal_top(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                ui.add_enabled_ui(available, |ui| {
                    egui::menu::menu_custom_button(ui, theme::primary("Download \u{23f7}"), |ui| {
                        if ui
                            .button("Original file")
                            .on_hover_text("Save this file unchanged, with its stored name")
                            .clicked()
                        {
                            choice = Some(HeaderAction::Save(None));
                            ui.close_menu();
                        }
                        if let Some(name) = &fixed {
                            let ext = name.rsplit('.').next().unwrap_or_default();
                            if ui
                                .button(format!("As .{ext}"))
                                .on_hover_text(format!(
                                    "Save a copy named {name}, for programs that go by extension"
                                ))
                                .clicked()
                            {
                                choice = Some(HeaderAction::Save(Some(name.clone())));
                                ui.close_menu();
                            }
                        }
                        if mesh {
                            ui.separator();
                            if ui
                                .button("OBJ for Blender\u{2026}")
                                .on_hover_text(
                                    "Export as OBJ scaled \u{d7}0.01 and turned Y-up, with \
                                     textures and a Make blend launcher that opens it in Blender.",
                                )
                                .clicked()
                            {
                                choice =
                                    Some(HeaderAction::ExportObj(crate::mesh::ObjMode::Blender));
                                ui.close_menu();
                            }
                            if ui
                                .button("OBJ unmodified\u{2026}")
                                .on_hover_text(
                                    "Export as OBJ in IMVU's native coordinates, exactly as in \
                                     the file (Z-up, unscaled). Textures keep their names.",
                                )
                                .clicked()
                            {
                                choice =
                                    Some(HeaderAction::ExportObj(crate::mesh::ObjMode::Unmodified));
                                ui.close_menu();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Choose how to save this file");
                });
            });
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(&file.name)
                    .strong()
                    .color(theme::FG_BRIGHT),
            )
            .wrap(),
        );
        choice
    }

    fn save_selected_file(&mut self, index: usize, name: Option<String>) {
        let Some(file) = self
            .selected_file
            .and_then(|f| self.packages[index].files.get(f))
        else {
            return;
        };
        let Ok(bytes) = &file.bytes else { return };
        let base = name.unwrap_or_else(|| {
            file.name
                .rsplit('/')
                .next()
                .unwrap_or(&file.name)
                .to_string()
        });
        if let Some(path) = rfd::FileDialog::new().set_file_name(&base).save_file() {
            self.status = match package::write_file(&path, bytes) {
                Ok(()) => format!(
                    "Saved {} ({} bytes, unchanged).",
                    path.display(),
                    bytes.len()
                ),
                Err(e) => format!("Could not save {}: {e}", path.display()),
            };
        }
    }

    fn save_restored(&mut self, index: usize) {
        let (restored, renamed) = self.packages[index].with_referenced_names();
        if renamed.is_empty() {
            self.status = "No file could be matched to a single referenced name.".into();
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!(
                "{}_restored.chkn",
                self.packages[index].save_name()
            ))
            .add_filter("chkn", &["chkn"])
            .save_file()
        else {
            return;
        };
        let unverifiable = self.packages[index].unreferenced_images();
        self.status = match package::write_file(&path, &package::zip_bytes(&restored)) {
            Ok(()) => format!(
                "Saved {} with {} file name(s) restored to what the package refers to: {}.{}",
                path.display(),
                renamed.len(),
                renamed
                    .iter()
                    .map(|(from, to)| format!("{} ➡ {}", display_name(from), display_name(to)))
                    .collect::<Vec<_>>()
                    .join(", "),
                if unverifiable.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Not referenced by anything, so kept as they are (their original \
                         names cannot be known from this copy): {}",
                        unverifiable
                            .iter()
                            .map(|n| display_name(n))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ),
            Err(e) => format!("Could not save {}: {e}", path.display()),
        };
    }

    fn save_images(&mut self, index: usize) {
        let Some(parent) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        self.status =
            match package::save_images_with_corrected_names(&self.packages[index], &parent) {
                Ok(copy) => format!(
                    "Saved {} image(s) to {}; {} renamed to match their contents \
                     (listed in renamed.txt). Not a working package: index.xml uses the \
                     original names.",
                    copy.saved,
                    copy.folder.display(),
                    copy.renamed.len()
                ),
                Err(e) => format!("Nothing saved: {e}"),
            };
    }

    fn save_all(&mut self, index: usize) {
        let Some(parent) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        self.status = match package::extract_all(&self.packages[index], &parent) {
            Ok((folder, skipped)) if skipped.is_empty() => {
                format!("Saved every file, unchanged, to {}.", folder.display())
            }
            Ok((folder, skipped)) => format!(
                "Saved to {}, but {} file(s) were not available: {}",
                folder.display(),
                skipped.len(),
                skipped.join(", ")
            ),
            Err(e) => format!("Nothing saved: {e}"),
        };
    }

    fn save_archive(&mut self, index: usize, extension: &str) {
        let package = &self.packages[index];
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.{extension}", package.save_name()))
            .add_filter(extension, &[extension])
            .save_file()
        {
            let bytes = package::zip_bytes(package);
            let missing = package.files.iter().filter(|f| f.bytes.is_err()).count();
            self.status = match package::write_file(&path, &bytes) {
                Ok(()) if missing == 0 => format!(
                    "Saved {} with {} original files.",
                    path.display(),
                    package.files.len()
                ),
                Ok(()) => format!(
                    "Saved {} but it is partial: {missing} file(s) were not available.",
                    path.display()
                ),
                Err(e) => format!("Could not save {}: {e}", path.display()),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_list_sorts_by_name_type_and_size() {
        let file =
            |name: &str, len: usize| package::PackageFile::new(name.into(), Ok(vec![b'x'; len]));
        let files = vec![file("b.png", 30), file("A.xml", 10), file("c.png", 20)];
        assert_eq!(sorted_files(&files, None), vec![0, 1, 2]);
        // Name ignores case.
        assert_eq!(
            sorted_files(&files, Some((SortBy::Name, false))),
            vec![1, 0, 2]
        );
        assert_eq!(
            sorted_files(&files, Some((SortBy::Name, true))),
            vec![2, 0, 1]
        );
        assert_eq!(
            sorted_files(&files, Some((SortBy::Size, false))),
            vec![1, 2, 0]
        );
        assert_eq!(
            sorted_files(&files, Some((SortBy::Size, true))),
            vec![0, 2, 1]
        );
        // Equal types fall back to name order.
        let types = sorted_files(&files, Some((SortBy::Type, false)));
        let pos = |i| types.iter().position(|&t| t == i).unwrap();
        assert!(pos(0) < pos(2));
    }

    #[test]
    fn thumbnails_only_come_from_imvu_hosts() {
        assert!(trusted_image_url(
            "https://userimages-akm.imvu.com/productdata/x.png"
        ));
        assert!(trusted_image_url("https://imvu.com/a.png"));
        assert!(!trusted_image_url("http://userimages-akm.imvu.com/a.png"));
        assert!(!trusted_image_url("https://imvu.com.evil.example/a.png"));
        assert!(!trusted_image_url("https://user@evil.example/.imvu.com/a"));
        assert!(!trusted_image_url("https://evilimvu.com/a.png"));
    }
}
