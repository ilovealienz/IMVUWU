#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Geometry code indexes several parallel arrays by vertex ID; explicit index
// loops read more clearly there than zipped iterators. Renderer entry points
// take the full camera/visibility state as separate arguments.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]
mod app;
mod avatars;
mod browser;
mod cmf;
mod fonts;
mod gizmo;
mod gpu;
mod gpu_wgpu;
mod imvu;
mod mesh;
mod model;
mod neck;
mod package;
mod pose;
mod pose_render;
mod poser;
mod protected;
mod render;
mod settings;
mod shortcuts;
mod theme;
mod xmf;
mod xpf;
/// `--imvu-check <report> [local.zip]`: live check of the Packages tab's
/// network path. Makes a few public requests to IMVU (no login).
fn imvu_check(report_path: &str, local_zip: Option<&str>) {
    use std::fmt::Write;
    let client = imvu::Client::default();
    let mut report = String::from("Packages tab live check (public IMVU endpoints, no login)\n");
    for id in [69190922, 29030566, 999_999_999] {
        match client.product(id) {
            Ok(p) => writeln!(
                report,
                "product {id}: name={:?} creator={:?} parent={:?}",
                p.name, p.creator, p.parent
            ),
            Err(e) => writeln!(report, "product {id}: {e}"),
        }
        .unwrap();
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let plan = imvu::ScanPlan {
        first: 1,
        last: 40,
        stop_after_missing: 5,
    };
    imvu::scan_revisions(client.clone(), 29030566, plan, cancel, sender);
    let mut found = Vec::new();
    for event in receiver {
        match event {
            imvu::ScanEvent::Checked(r, imvu::RevisionState::Exists) => found.push(r),
            imvu::ScanEvent::Checked(r, imvu::RevisionState::Unknown(e)) => {
                writeln!(report, "revision {r}: could not check ({e})").unwrap()
            }
            imvu::ScanEvent::Finished { last_checked, .. } => {
                found.sort();
                writeln!(
                    report,
                    "29030566 scan: checked 1-{last_checked}, found {found:?}"
                )
                .unwrap();
            }
            _ => {}
        }
    }
    match client.contents(69190922, 1) {
        Ok(entries) => {
            let no_cancel = std::sync::atomic::AtomicBool::new(false);
            let files =
                imvu::download_revision(&client, 69190922, 1, &entries, &no_cancel, &|_| {});
            let remote = package::from_download(69190922, 1, files);
            writeln!(
                report,
                "69190922 rev 1: {} files, complete={}",
                remote.files.len(),
                remote.is_complete()
            )
            .unwrap();
            for file in &remote.files {
                writeln!(
                    report,
                    "  {}: {} (server sent {:?}), name mismatch: {:?}",
                    file.name,
                    file.kind
                        .as_ref()
                        .map(package::Kind::label)
                        .unwrap_or_default(),
                    file.server_type,
                    file.extension_mismatch()
                )
                .unwrap();
            }
            if let Some(Ok(index)) = remote.index() {
                writeln!(
                    report,
                    "  index imports {:?}, deletes {:?}",
                    index.imports, index.deletions
                )
                .unwrap();
            }
            if let Some(local) = local_zip {
                match package::open_archive(std::path::Path::new(local)) {
                    Ok(local) => {
                        for file in &remote.files {
                            let same = local
                                .files
                                .iter()
                                .find(|f| f.name == file.name)
                                .map(|f| f.bytes.as_ref().ok() == file.bytes.as_ref().ok());
                            writeln!(report, "  {} matches local ZIP: {same:?}", file.name)
                                .unwrap();
                        }
                    }
                    Err(e) => writeln!(report, "  local ZIP could not be read: {e}").unwrap(),
                }
            }
        }
        Err(e) => writeln!(report, "69190922 rev 1 contents: {e}").unwrap(),
    }
    std::fs::write(report_path, report).expect("Write report");
}

/// `--package-report <report> <item>...` where an item is a local ZIP/CHKN
/// path, a folder, or `product:revision` (downloaded from IMVU), optionally
/// followed by `=reference.zip` to compare the image copy with fixed names
/// against an archive made by another downloader.
fn package_report(report_path: &str, items: &[String]) {
    use std::fmt::Write;
    let client = imvu::Client::default();
    let mut report = String::new();
    #[derive(Default, Debug)]
    struct Totals {
        packages: usize,
        files: usize,
        images: usize,
        misnamed: usize,
        problems: usize,
        server_disagreements: usize,
        broken_references: usize,
    }
    let mut totals = Totals::default();
    for item in items {
        // `archive~product:revision`: restore the archive's original names
        // and compare it, file by file, with the live revision.
        if let Some((path, live)) = item.split_once('~') {
            let parsed = live
                .split_once(':')
                .and_then(|(p, r)| Some((p.parse::<u64>().ok()?, r.parse::<u32>().ok()?)));
            let (Some((p, r)), Ok(local)) =
                (parsed, package::open_archive(std::path::Path::new(path)))
            else {
                writeln!(report, "== restore {item}: could not open").unwrap();
                continue;
            };
            let (restored, renamed) = local.with_referenced_names();
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let remote = client
                .contents(p, r)
                .map(|entries| imvu::download_revision(&client, p, r, &entries, &cancel, &|_| {}));
            let Ok(remote) = remote else {
                writeln!(report, "== restore {item}: live revision unavailable").unwrap();
                continue;
            };
            let mut same = 0;
            let mut differences = Vec::new();
            for live_file in &remote {
                let Ok(live_bytes) = &live_file.result else {
                    continue;
                };
                match restored
                    .files
                    .iter()
                    .find(|f| f.name == live_file.entry.name)
                {
                    Some(f) if f.bytes.as_ref().ok().map(|b| b.as_slice()) == Some(live_bytes) => {
                        same += 1
                    }
                    Some(_) => differences.push(format!("{} (bytes differ)", live_file.entry.name)),
                    None => differences.push(format!(
                        "{} (name not in restored copy)",
                        live_file.entry.name
                    )),
                }
            }
            writeln!(
                report,
                "== restore {path} vs live {p}:{r}: {} renamed, {same} of {} live files identical by name and bytes; extra files in archive: {}",
                renamed.len(),
                remote.len(),
                restored.files.len().saturating_sub(same)
            )
            .unwrap();
            for d in differences {
                writeln!(report, "  differs: {d}").unwrap();
            }
            continue;
        }
        totals.packages += 1;
        let (source, reference) = match item.split_once('=') {
            Some((s, r)) => (s, Some(r)),
            None => (item.as_str(), None),
        };
        // A bare product ID means its newest revision found by a scan.
        let remote = source
            .split_once(':')
            .and_then(|(p, r)| Some((p.parse::<u64>().ok()?, r.parse::<u32>().ok()?)))
            .or_else(|| {
                let id = source.parse::<u64>().ok()?;
                let (sender, receiver) = std::sync::mpsc::channel();
                let plan = imvu::ScanPlan {
                    first: 1,
                    last: 60,
                    stop_after_missing: 8,
                };
                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                imvu::scan_revisions(client.clone(), id, plan, cancel, sender);
                let newest = receiver
                    .into_iter()
                    .filter_map(|e| match e {
                        imvu::ScanEvent::Checked(r, imvu::RevisionState::Exists) => Some(r),
                        _ => None,
                    })
                    .max();
                Some((id, newest.unwrap_or(0)))
            });
        let opened = if let Some((p, r)) = remote {
            if r == 0 {
                writeln!(report, "== {source}\n  no revisions found in 1-60").unwrap();
                continue;
            }
            client
                .contents(p, r)
                .map_err(|e| e.to_string())
                .map(|entries| {
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    let files = imvu::download_revision(&client, p, r, &entries, &cancel, &|_| {});
                    package::from_download(p, r, files)
                })
        } else {
            let path = std::path::Path::new(source);
            if path.is_dir() {
                package::open_folder(path)
            } else {
                package::open_archive(path)
            }
        };
        match remote {
            Some((p, r)) => writeln!(report, "== {p} revision {r}").unwrap(),
            None => writeln!(report, "== {source}").unwrap(),
        }
        let pkg = match opened {
            Ok(p) => p,
            Err(e) => {
                writeln!(report, "  could not open: {e}").unwrap();
                continue;
            }
        };
        for note in &pkg.notes {
            writeln!(report, "  note: {note}").unwrap();
        }
        for f in &pkg.files {
            writeln!(
                report,
                "  {:<52} {:<22} server={:<12} mismatch={:<8} fixed={}",
                f.name,
                f.kind
                    .as_ref()
                    .map(package::Kind::label)
                    .unwrap_or_else(|| { f.bytes.as_ref().err().cloned().unwrap_or_default() }),
                f.server_type.as_deref().unwrap_or("-"),
                f.extension_mismatch()
                    .map(|e| if e.is_empty() {
                        "(none)".into()
                    } else {
                        format!(".{e}")
                    })
                    .unwrap_or_else(|| "-".into()),
                f.corrected_name().unwrap_or_else(|| "-".into())
            )
            .unwrap();
            // Cross-check detection against an actual decode (signature
            // guess, then TGA): any disagreement is a detection bug.
            totals.files += 1;
            let Ok(bytes) = &f.bytes else { continue };
            let decoded = image::load_from_memory(bytes)
                .ok()
                .map(|_| format!("{:?}", image::guess_format(bytes).unwrap()))
                .or_else(|| {
                    image::load_from_memory_with_format(bytes, image::ImageFormat::Tga)
                        .ok()
                        .map(|_| "Tga".to_string())
                });
            let detected_image = f.kind.as_ref().is_some_and(package::Kind::is_image);
            if detected_image {
                totals.images += 1;
            }
            if f.extension_mismatch().is_some() {
                totals.misnamed += 1;
            }
            let problem = match (&decoded, detected_image) {
                (Some(format), false) => {
                    Some(format!("decodes as {format} but detected as non-image"))
                }
                (None, true) => Some("detected as image but does not decode".into()),
                (Some(format), true) => {
                    let label = f
                        .kind
                        .as_ref()
                        .map(package::Kind::label)
                        .unwrap_or_default();
                    (!label
                        .to_lowercase()
                        .starts_with(&format.to_lowercase()[..3]))
                    .then(|| format!("detected {label} but decodes as {format}"))
                }
                (None, false) => None,
            };
            let server_image = f
                .server_type
                .as_deref()
                .is_some_and(|t| t.starts_with("image/"));
            if f.server_type.is_some() && server_image != detected_image {
                writeln!(
                    report,
                    "    note: server type {:?} vs detected {:?}",
                    f.server_type,
                    f.kind.as_ref().map(package::Kind::label)
                )
                .unwrap();
                totals.server_disagreements += 1;
            }
            if let Some(problem) = problem {
                writeln!(report, "    PROBLEM: {problem}").unwrap();
                totals.problems += 1;
            }
        }
        let refs = pkg.references();
        let mut counts = std::collections::BTreeMap::new();
        for r in &refs {
            let key = match &r.state {
                package::RefState::Present => "present",
                package::RefState::NotHere => "not here (inherited or missing)",
                package::RefState::OtherExtension(_) => "under another extension",
                package::RefState::AddedExtension(_) => "with an extension added",
            };
            *counts.entry(key).or_insert(0) += 1;
            if matches!(
                r.state,
                package::RefState::OtherExtension(_) | package::RefState::AddedExtension(_)
            ) {
                totals.broken_references += 1;
            }
            if !matches!(r.state, package::RefState::Present) {
                writeln!(report, "  ref {} -> {:?}: {:?}", r.from, r.target, r.state).unwrap();
            }
        }
        writeln!(report, "  references: {counts:?}").unwrap();
        if let Some(reference) = reference {
            match package::open_archive(std::path::Path::new(reference)) {
                Ok(other) => {
                    let mut same = 0;
                    for f in pkg
                        .files
                        .iter()
                        .filter(|f| f.kind.as_ref().is_some_and(package::Kind::is_image))
                    {
                        let name = f.corrected_name().unwrap_or_else(|| f.name.clone());
                        let theirs = other.files.iter().find(|o| o.name == name);
                        match theirs {
                            Some(o) if o.bytes.as_ref().ok() == f.bytes.as_ref().ok() => same += 1,
                            Some(_) => writeln!(report, "  compare {name}: bytes differ").unwrap(),
                            None => {
                                writeln!(report, "  compare {name}: not in {reference}").unwrap()
                            }
                        }
                    }
                    writeln!(
                        report,
                        "  compare with {reference}: {same} image(s) match by fixed name and bytes"
                    )
                    .unwrap();
                }
                Err(e) => writeln!(report, "  compare: could not open {reference}: {e}").unwrap(),
            }
        }
    }
    writeln!(report, "\nTOTALS {totals:?}").unwrap();
    std::fs::write(report_path, report).expect("Write report");
}

fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    // `--build-avatars <folder>`: downloads products 80/191 into `<folder>`
    // (an app-data root) exactly as Settings does, and reports the result.
    if args.get(1).is_some_and(|s| s == "--build-avatars") {
        let root = std::path::PathBuf::from(args.get(2).expect("Folder"));
        let client = imvu::Client::default();
        let mut report = String::new();
        for female in [true, false] {
            let result = avatars::build_from_imvu(&client, &root, female, &|_| {});
            let loaded = avatars::installed(&root, female);
            report.push_str(&format!(
                "{}: {:?}; installed files: {}; mesh loads: {}
",
                if female { "female" } else { "male" },
                result,
                loaded.as_ref().map_or(0, |p| p.files.len()),
                loaded
                    .as_ref()
                    .is_some_and(|p| avatars::validate(&p.slices(), female).is_ok())
            ));
        }
        std::fs::write(root.join("build-report.txt"), report).expect("Write report");
        return Ok(());
    }
    // `--export-obj <mesh> <folder> <blender|unmodified>`: the Packages tab's
    // OBJ export for a mesh file (no textures), for checking in Blender.
    if args.get(1).is_some_and(|s| s == "--export-obj") {
        let source = std::path::Path::new(args.get(2).expect("Mesh file"));
        let folder = std::path::Path::new(args.get(3).expect("Folder"));
        let mode = match args.get(4).map(String::as_str) {
            Some("unmodified") => mesh::ObjMode::Unmodified,
            _ => mesh::ObjMode::Blender,
        };
        let parsed = mesh::StaticMesh::parse(&std::fs::read(source).expect("Read mesh"))
            .expect("Parse mesh");
        let stem = source.file_stem().unwrap().to_string_lossy();
        let (obj, _) = mesh::to_obj(&parsed, &stem, mode, &Default::default());
        std::fs::create_dir_all(folder).expect("Create folder");
        std::fs::write(folder.join(format!("{stem}.obj")), obj).expect("Write OBJ");
        for (name, text) in mesh::blend_launcher(&stem, mode) {
            std::fs::write(folder.join(name), text).expect("Write Blender launcher");
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--package-report") {
        package_report(
            args.get(2).expect("Report path"),
            &args[3.min(args.len())..],
        );
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--imvu-check") {
        imvu_check(
            args.get(2).expect("Report path"),
            args.get(3).map(String::as_str),
        );
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|s| s == "--preview-performance-check")
    {
        let skeleton = pose::Skeleton::bundled();
        let mesh = pose_render::AvatarMesh::female();
        let pose = skeleton.rest_pose();
        let camera = render::Camera {
            zoom: 0.8,
            ..Default::default()
        };
        let size = eframe::egui::vec2(3200.0, 1800.0);
        let moving = render::preview_size_for_activity(size, 1.0, true);
        let idle = render::preview_size_for_activity(size, 1.0, false);
        let mut report=String::from("Female poser software render, same camera/assets, single samples; elapsed includes AA resize. Not a live interaction FPS measurement.\n");
        for (name, resolution, display) in [
            (
                "Full quality 4K-sized view (pre-0.14 mode)",
                [5333, 3000],
                [3200, 1800],
            ),
            ("Interacting", moving.render, moving.display),
            ("Settled", idle.render, idle.display),
        ] {
            let start = std::time::Instant::now();
            let frame = mesh.render(&skeleton, std::slice::from_ref(&pose), &camera, resolution);
            let raster = start.elapsed();
            let image = render::antialias_preview(frame.image, display);
            std::hint::black_box(image);
            let total = start.elapsed();
            report.push_str(&format!(
                "{name}: render={resolution:?}, display={display:?}, elapsed_ms={:.2} \
                 (raster {:.2}, AA resize {:.2})\n",
                total.as_secs_f64() * 1000.0,
                raster.as_secs_f64() * 1000.0,
                (total - raster).as_secs_f64() * 1000.0
            ));
        }
        // CPU-side work behind each GPU-preview redraw (the default path),
        // averaged over repeated runs.
        let average_ms = |runs: u32, mut work: Box<dyn FnMut() + '_>| {
            work();
            let start = std::time::Instant::now();
            for _ in 0..runs {
                work();
            }
            start.elapsed().as_secs_f64() * 1000.0 / runs as f64
        };
        let male = pose_render::AvatarMesh::male();
        let view = [1886, 1061];
        let single = average_ms(
            30,
            Box::new(|| {
                std::hint::black_box(pose_render::AvatarMesh::gpu_scene(
                    &skeleton,
                    &[(&mesh, &pose)],
                    &camera,
                    view,
                ));
            }),
        );
        let couple = average_ms(
            30,
            Box::new(|| {
                std::hint::black_box(pose_render::AvatarMesh::gpu_scene(
                    &skeleton,
                    &[(&mesh, &pose), (&male, &pose)],
                    &camera,
                    view,
                ));
            }),
        );
        let doc = model::Document::template(true);
        let textures = render::textures(true);
        let hidden_parts = vec![false; doc.parts.len()];
        let hidden = vec![false; doc.positions().len()];
        let head = average_ms(
            30,
            Box::new(|| {
                std::hint::black_box(gpu::Scene::head(
                    &doc,
                    &textures,
                    doc.positions(),
                    &camera,
                    view,
                    &hidden_parts,
                    &hidden,
                    true,
                ));
            }),
        );
        report.push_str(&format!(
            "GPU preview CPU prep, {view:?}, mean of 30: poser single={single:.2} ms, \
             poser couple={couple:.2} ms, head={head:.2} ms\n"
        ));
        std::fs::write(args.get(2).expect("Report path"), report)
            .expect("Write performance report");
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--poser-render-check") {
        let folder = std::path::Path::new(args.get(2).expect("Output folder"));
        std::fs::create_dir_all(folder).expect("Create preview folder");
        let skeleton = pose::Skeleton::bundled();
        let mesh = pose_render::AvatarMesh::bundled();
        let neutral = skeleton.rest_pose();
        let mut bent = neutral.clone();
        bent.local[27].rotation =
            bent.local[27]
                .rotation
                .mul(poser::joint_rotation(27, model::V(0.0, 0.0, 90.0), false));
        let mut left = neutral.clone();
        let mut right = bent.clone();
        left.local[0].position.0 = -180.0;
        right.local[0].position.0 = 180.0;
        for (name, actors, camera) in [
            ("neutral", vec![neutral], render::Camera::default()),
            (
                "elbow",
                vec![bent.clone()],
                render::Camera {
                    yaw: 0.65,
                    ..Default::default()
                },
            ),
            ("elbow-front", vec![bent.clone()], render::Camera::default()),
            (
                "elbow-side",
                vec![bent.clone()],
                render::Camera {
                    yaw: std::f32::consts::FRAC_PI_2,
                    ..Default::default()
                },
            ),
            (
                "couple",
                vec![left, right],
                render::Camera {
                    zoom: 0.85,
                    ..Default::default()
                },
            ),
        ] {
            let frame = mesh.render(&skeleton, &actors, &camera, [640, 720]);
            let pixels: Vec<u8> = frame
                .image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect();
            image::save_buffer(
                folder.join(format!("{name}.png")),
                &pixels,
                640,
                720,
                image::ColorType::Rgba8,
            )
            .expect("Write avatar preview");
        }
        let female = pose_render::AvatarMesh::female();
        let male = pose_render::AvatarMesh::male();
        let female_pose = skeleton.rest_pose();
        let mut male_pose = female_pose.clone();
        male_pose.local[58].rotation = male_pose.local[58].rotation.mul(poser::joint_rotation(
            58,
            model::V(0.0, 0.0, 75.0),
            false,
        ));
        let mut a = female_pose.clone();
        let mut b = male_pose.clone();
        a.local[1].position.0 = -180.0;
        b.local[1].position.0 = 180.0;
        for (name, actors) in [
            ("female", vec![(&female, &female_pose)]),
            ("male", vec![(&male, &male_pose)]),
            ("mixed-couple", vec![(&female, &a), (&male, &b)]),
        ] {
            let frame = pose_render::AvatarMesh::render_actors(
                &skeleton,
                &actors,
                &render::Camera {
                    zoom: 0.8,
                    ..Default::default()
                },
                [640, 720],
            );
            let pixels: Vec<u8> = frame
                .image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect();
            image::save_buffer(
                folder.join(format!("{name}.png")),
                &pixels,
                640,
                720,
                image::ColorType::Rgba8,
            )
            .expect("Write supplied avatar preview");
        }
        for (name, yaw) in [
            ("female-three-quarter", 0.65),
            ("female-back", std::f32::consts::PI),
        ] {
            let frame = pose_render::AvatarMesh::render_actors(
                &skeleton,
                &[(&female, &female_pose)],
                &render::Camera {
                    zoom: 0.8,
                    yaw,
                    ..Default::default()
                },
                [640, 720],
            );
            let pixels: Vec<u8> = frame
                .image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect();
            image::save_buffer(
                folder.join(format!("{name}.png")),
                &pixels,
                640,
                720,
                image::ColorType::Rgba8,
            )
            .expect("Write hair check");
        }
        for (name, mesh) in [("female-head", &female), ("male-head", &male)] {
            let mut camera = render::Camera {
                zoom: 3.2,
                yaw: 0.35,
                ..Default::default()
            };
            let head = skeleton.world(&female_pose)[22]
                .position
                .add(model::V(0.0, 0.0, 75.0));
            let p = camera.project(pose_render::view_point(head), 640, 720);
            camera.pan = [320.0 - p[0], 330.0 - p[1]];
            let frame = pose_render::AvatarMesh::render_actors(
                &skeleton,
                &[(mesh, &female_pose)],
                &camera,
                [640, 720],
            );
            let pixels: Vec<u8> = frame
                .image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect();
            image::save_buffer(
                folder.join(format!("{name}.png")),
                &pixels,
                640,
                720,
                image::ColorType::Rgba8,
            )
            .expect("Write eyebrow check");
        }
        let exported = bent
            .export_xaf(&skeleton, 1.0, false)
            .expect("Export elbow pose");
        pose::Pose::load_xaf(&skeleton, exported.as_bytes(), 0.0)
            .expect("Validate exported elbow XAF");
        std::fs::write(folder.join("elbow.xaf"), exported).expect("Write elbow pose");
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--expression-check") {
        let folder = std::path::Path::new(args.get(2).expect("Output folder"));
        std::fs::create_dir_all(folder).expect("Create output folder");
        let doc = model::Document::template(true);
        let mut animation = xpf::Animation::default();
        animation.set_weight("mouth.Open.Exclusive", 0.0, 0.7);
        std::fs::write(
            folder.join("Mouth-open.xpf"),
            animation.export().expect("Export XPF"),
        )
        .expect("Write XPF");
        for (name, positions) in [
            ("neutral", xpf::Animation::default().blend(&doc, 0.0)),
            ("expression", animation.blend(&doc, 0.0)),
        ] {
            let frame = render::render_positions(
                &doc,
                &render::textures(true),
                &render::Camera::default(),
                [640, 720],
                &vec![false; doc.parts.len()],
                &vec![false; positions.len()],
                true,
                &positions,
            );
            let pixels: Vec<u8> = frame
                .image
                .pixels
                .iter()
                .flat_map(|p| p.to_array())
                .collect();
            image::save_buffer(
                folder.join(format!("{name}.png")),
                &pixels,
                640,
                720,
                image::ColorType::Rgba8,
            )
            .expect("Write preview");
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--preview-mesh") {
        let bytes = std::fs::read(args.get(2).expect("Input mesh")).expect("Read mesh");
        let d = model::Document::load(&bytes, true).expect("Import mesh");
        let f = render::render(
            &d,
            &render::textures(true),
            &render::Camera::default(),
            [640, 720],
            &vec![false; d.parts.len()],
            &vec![false; d.positions().len()],
            true,
        );
        let pixels: Vec<u8> = f.image.pixels.iter().flat_map(|p| p.to_array()).collect();
        image::save_buffer(
            args.get(3).expect("Output PNG"),
            &pixels,
            640,
            720,
            image::ColorType::Rgba8,
        )
        .expect("Save preview");
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--repair-export") {
        let source =
            std::fs::read_to_string(args.get(2).expect("Provide input XMF")).expect("Read XMF");
        let doc = model::Document::parse(&source, true).expect("Parse input mesh");
        let xml = doc.export().expect("Export ordered Cal3D XMF");
        let path = args.get(3).expect("Provide output XMF");
        std::fs::write(path, xml).expect("Write repaired XMF");
        println!("Saved repaired export: {path}");
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|s| s == "--export-female" || s == "--export-male")
    {
        let path = args.get(2).expect("Provide an output XMF path");
        let xml = model::Document::template(args[1] == "--export-female")
            .export()
            .expect("XMF export");
        std::fs::write(path, xml).expect("Write female XMF");
        println!("Saved repaired main head: {path}");
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--render-check") {
        render::render_check(args.get(2).map(String::as_str).unwrap_or("analysis"))
            .expect("Render check failed");
        return Ok(());
    }
    // `--font-report <file>`: which system fonts the app would use as
    // fallbacks for scripts and symbols its bundled fonts lack.
    if args.get(1).is_some_and(|s| s == "--font-report") {
        let report = fonts::report();
        match args.get(2) {
            Some(path) => std::fs::write(path, &report).expect("Write font report"),
            None => print!("{report}"),
        }
        return Ok(());
    }
    // `--wgpu-render-check <folder> [--software]`: the wgpu preview, headless.
    if args.get(1).is_some_and(|s| s == "--wgpu-render-check") {
        let folder = std::path::PathBuf::from(args.get(2).expect("Output folder"));
        let software = args.iter().any(|a| a == "--software");
        if let Err(e) = gpu_wgpu::run_checks(&folder, software) {
            let _ = std::fs::create_dir_all(&folder);
            std::fs::write(folder.join("ERROR.txt"), e).expect("Write GPU error");
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--gpu-render-check") {
        struct Check;
        impl eframe::App for Check {
            fn update(&mut self, ctx: &eframe::egui::Context, _: &mut eframe::Frame) {
                ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            }
        }
        let folder = std::path::PathBuf::from(args.get(2).expect("Output folder"));
        std::fs::create_dir_all(&folder).expect("Create GPU check folder");
        return eframe::run_native(
            "GPU renderer diagnostic",
            eframe::NativeOptions {
                // The GPU checks use OpenGL directly; with the wgpu feature
                // also enabled, eframe's default renderer would be wgpu.
                renderer: eframe::Renderer::Glow,
                depth_buffer: 24,
                multisampling: 2,
                viewport: eframe::egui::ViewportBuilder::default().with_visible(false),
                ..Default::default()
            },
            Box::new(move |cc| {
                let result = cc
                    .gl
                    .clone()
                    .ok_or("No GL context".into())
                    .and_then(|gl| gpu::run_checks(gl, &folder));
                if let Err(e) = result {
                    std::fs::write(folder.join("ERROR.txt"), e).expect("Write GPU error");
                }
                Ok(Box::new(Check))
            }),
        );
    }
    run_app(&args)
}

/// How the window is drawn. The app tries them in order: wgpu (DirectX 12,
/// Vulkan, Metal or OpenGL, whichever works; on Windows it also runs on the
/// basic display driver through DirectX's software adapter), then OpenGL
/// with multisampling, then OpenGL without it (some drivers offer no
/// multisampled format). Both GPU paths draw the 3D preview on the GPU.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Renderer {
    Glow,
    GlowBasic,
    Wgpu,
}

impl Renderer {
    const ORDER: [Renderer; 3] = [Renderer::Wgpu, Renderer::Glow, Renderer::GlowBasic];
    fn name(self) -> &'static str {
        match self {
            Renderer::Glow => "glow",
            Renderer::GlowBasic => "glow-basic",
            Renderer::Wgpu => "wgpu",
        }
    }
}

fn run_app(args: &[String]) -> eframe::Result {
    install_crash_report();
    let renderer = args
        .iter()
        .position(|a| a == "--renderer")
        .and_then(|i| args.get(i + 1))
        .and_then(|name| Renderer::ORDER.into_iter().find(|r| r.name() == name))
        .unwrap_or(Renderer::Wgpu);
    let mut options = eframe::NativeOptions {
        renderer: if renderer == Renderer::Wgpu {
            eframe::Renderer::Wgpu
        } else {
            eframe::Renderer::Glow
        },
        // wgpu: the 3D preview has its own depth and 4x MSAA offscreen,
        // so egui's own pass needs neither.
        depth_buffer: if renderer == Renderer::Wgpu { 0 } else { 24 },
        multisampling: if renderer == Renderer::Glow { 2 } else { 0 },
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([1000.0, 680.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    // `--software-gpu`: wgpu on the CPU adapter (Windows' "Microsoft Basic
    // Render Driver"), as on a PC with no graphics driver, to test that case
    // on any machine.
    if args.iter().any(|a| a == "--software-gpu") {
        options.wgpu_options.wgpu_setup =
            eframe::egui_wgpu::WgpuSetup::CreateNew(eframe::egui_wgpu::WgpuSetupCreateNew {
                native_adapter_selector: Some(std::sync::Arc::new(|adapters, _surface| {
                    adapters
                        .iter()
                        .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
                        .cloned()
                        .ok_or_else(|| "No software (CPU) graphics adapter".to_string())
                })),
                ..Default::default()
            });
    }
    // IMVUWU_FAIL_RENDERER=glow,glow-basic makes those renderers fail, to
    // check the fallback chain on a machine where they would work.
    let forced_failure = std::env::var("IMVUWU_FAIL_RENDERER")
        .is_ok_and(|v| v.split(',').any(|n| n.trim() == renderer.name()));
    let result = if forced_failure {
        Err(eframe::Error::AppCreation(
            "failure forced by IMVUWU_FAIL_RENDERER".into(),
        ))
    } else {
        eframe::run_native(
            concat!("IMVUWU v", env!("CARGO_PKG_VERSION")),
            options,
            Box::new(|cc| Ok(Box::new(app::EditorApp::new(cc)))),
        )
    };
    let Err(error) = result else {
        return Ok(());
    };
    log_startup(&format!("Renderer {} failed: {error}", renderer.name()));
    // A window system can only be started once per process, so the next
    // renderer runs in a fresh copy of the app with the same arguments.
    let next = Renderer::ORDER
        .into_iter()
        .skip_while(|r| *r != renderer)
        .nth(1);
    if let Some(next) = next {
        if let Ok(exe) = std::env::current_exe() {
            let mut rest: Vec<String> = Vec::new();
            let mut skip = false;
            for a in args.iter().skip(1) {
                if skip {
                    skip = false;
                } else if a == "--renderer" {
                    skip = true;
                } else {
                    rest.push(a.clone());
                }
            }
            let spawned = std::process::Command::new(exe)
                .args(rest)
                .arg("--renderer")
                .arg(next.name())
                .spawn();
            if spawned.is_ok() {
                return Ok(());
            }
        }
    }
    show_error(&format!(
        "IMVUWU could not open its window.\n\n{error}\n\nUpdating the graphics driver \
         usually fixes this. Details are in {}.",
        log_path().display()
    ));
    Err(error)
}

/// Where start-up problems and crashes are written: the app folder, or the
/// temp folder when APPDATA is not set.
fn log_path() -> std::path::PathBuf {
    settings::app_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("startup-log.txt")
}

fn log_startup(text: &str) {
    use std::io::Write;
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(
            file,
            "[{secs}] IMVUWU {}: {text}",
            env!("CARGO_PKG_VERSION")
        );
    }
}

/// A crash shows a message and is logged, instead of the window silently
/// disappearing (the release exe has no console).
fn install_crash_report() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_startup(&format!("Crash: {info}"));
        show_error(&format!(
            "IMVUWU stopped because of an error:\n\n{info}\n\nDetails are in {}.",
            log_path().display()
        ));
        default(info);
    }));
}

/// An error message box (Windows, Linux and macOS, through rfd), also
/// printed for anyone running from a terminal.
fn show_error(text: &str) {
    eprintln!("{text}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("IMVUWU")
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// The window/taskbar icon (the exe file icon comes from build.rs).
fn app_icon() -> eframe::egui::IconData {
    let image = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .expect("Bundled icon")
        .into_rgba8();
    let (width, height) = image.dimensions();
    eframe::egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}
