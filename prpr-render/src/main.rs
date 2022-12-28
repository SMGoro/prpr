mod scene;

use crate::scene::MainScene;
use anyhow::{bail, Context, Result};
use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
use macroquad::{miniquad::TextureFormat, prelude::*};
use prpr::{
    audio::AudioClip,
    build_conf,
    config::Config,
    core::{init_assets, NoteKind},
    fs::{self, PatchedFileSystem},
    scene::{GameScene, LoadingScene},
    time::TimeManager,
    ui::{ChartInfoEdit, Ui},
    Main,
};
use prpr::{ext::screen_aspect, scene::BILLBOARD};
use std::fmt::Write as _;
use std::{
    cell::RefCell,
    io::{BufWriter, Cursor, Write},
    ops::Deref,
    process::{Command, Stdio},
    rc::Rc,
    sync::Mutex,
    time::Instant,
};

#[derive(Clone)]
struct VideoConfig {
    fps: u32,
    resolution: (u32, u32),
    hardware_accel: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            fps: 60,
            resolution: (1920, 1080),
            hardware_accel: false,
        }
    }
}

static INFO_EDIT: Mutex<Option<ChartInfoEdit>> = Mutex::new(None);
static VIDEO_CONFIG: Mutex<Option<VideoConfig>> = Mutex::new(None);

#[cfg(target_arch = "wasm32")]
compile_error!("WASM target is not supported");

#[macroquad::main(build_conf)]
async fn main() -> Result<()> {
    init_assets();

    let Ok(exe) = std::env::current_exe() else {
        bail!("找不到当前可执行程序");
    };
    let exe_dir = exe.parent().unwrap();
    let ffmpeg = if cfg!(target_os = "windows") {
        let local = exe_dir.join("ffmpeg.exe");
        if local.exists() {
            local.display().to_string()
        } else {
            "ffmpeg".to_owned()
        }
    } else {
        "ffmpeg".to_owned()
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let _ = prpr::ui::FONT.set(load_ttf_font("font.ttf").await?);

    let (path, config) = {
        let mut args = std::env::args().skip(1);
        let Some(path) = args.next() else {
            bail!("请将谱面文件或文件夹拖动到该软件上！");
        };
        let config = match (|| -> Result<Config> {
            Ok(serde_yaml::from_str(
                &std::fs::read_to_string(exe_dir.join("conf.yml"))
                    .or_else(|_| std::fs::read_to_string("conf.yml"))
                    .context("无法加载配置文件")?,
            )?)
        })() {
            Err(err) => {
                warn!("无法加载配置文件：{:?}", err);
                Config::default()
            }
            Ok(config) => config,
        };
        (path, config)
    };

    let (info, mut fs) = fs::load_info(fs::fs_from_file(std::path::Path::new(&path))?).await?;

    let chart = GameScene::load_chart(&mut fs, &info).await?;
    macro_rules! ld {
        ($path:literal) => {
            StaticSoundData::from_cursor(Cursor::new(load_file($path).await?), StaticSoundSettings::default())?
        };
    }
    let music = StaticSoundData::from_cursor(Cursor::new(fs.load_file(&info.music).await?), StaticSoundSettings::default())?;
    let ending = StaticSoundData::from_cursor(Cursor::new(load_file("ending.mp3").await?), StaticSoundSettings::default())?;
    let track_length = music.frames.len() as f64 / music.sample_rate as f64;
    let sfx_click = ld!("click.ogg");
    let sfx_drag = ld!("drag.ogg");
    let sfx_flick = ld!("flick.ogg");

    let mut gl = unsafe { get_internal_gl() };

    let texture = miniquad::Texture::new_render_texture(
        gl.quad_context,
        miniquad::TextureParams {
            width: 1080,
            height: 608,
            format: TextureFormat::RGB8,
            ..Default::default()
        },
    );
    let target = Some({
        let render_pass = miniquad::RenderPass::new(gl.quad_context, texture, None);
        RenderTarget {
            texture: Texture2D::from_miniquad_texture(texture),
            render_pass,
        }
    });
    let tex = Texture2D::from_miniquad_texture(texture);
    let mut main = Main::new(Box::new(MainScene::new(target, info, config.clone(), fs.clone_box())), TimeManager::default(), None)?;
    let width = texture.width as f32 / 2.;
    loop {
        main.update()?;
        if main.scenes.len() == 1 {
            gl.quad_gl.viewport(Some((0, 0, texture.width as _, texture.height as _)));
            let mut ui = Ui::new();
            let sw = screen_width();
            let lf = (sw - width) / 2.;
            ui.mutate_touches(|touch| {
                touch.position.x -= lf / texture.width as f32 * 2.;
            });
            main.show_billboard = false;
            main.render(&mut ui)?;
            gl.flush();
            set_camera(&Camera2D {
                zoom: vec2(1., -screen_aspect()),
                ..Default::default()
            });
            let mut ui = Ui::new();
            clear_background(GRAY);
            draw_texture_ex(
                tex,
                -1. + lf / sw * 2.,
                -ui.top,
                WHITE,
                DrawTextureParams {
                    flip_y: true,
                    dest_size: Some(vec2(texture.width as f32, texture.height as f32) * (2. / sw)),
                    ..Default::default()
                },
            );
            BILLBOARD.with(|it| {
                let mut guard = it.borrow_mut();
                let t = guard.1.now() as f32;
                guard.0.render(&mut ui, t);
            });
        } else {
            gl.quad_gl.viewport(None);
            gl.quad_gl.render_pass(None);
            set_default_camera();
            main.render(&mut Ui::new())?;
        }
        if main.should_exit() {
            break;
        }

        next_frame().await;
    }
    clear_background(BLACK);
    next_frame().await;

    let edit = INFO_EDIT.lock().unwrap().take().unwrap();
    let config = Config {
        autoplay: true,
        volume_music: 0.,
        volume_sfx: 0.,
        ..config
    };

    let v_config = VIDEO_CONFIG.lock().unwrap().take().unwrap();
    let (vw, vh) = v_config.resolution;

    let texture = miniquad::Texture::new_render_texture(
        gl.quad_context,
        miniquad::TextureParams {
            width: vw,
            height: vh,
            format: TextureFormat::RGB8,
            ..Default::default()
        },
    );
    let target = Some({
        let render_pass = miniquad::RenderPass::new(gl.quad_context, texture, None);
        RenderTarget {
            texture: Texture2D::from_miniquad_texture(texture),
            render_pass,
        }
    });

    info!("[1] 渲染视频…");

    let my_time: Rc<RefCell<f64>> = Rc::new(RefCell::new(0.));
    let tm = TimeManager::manual(Box::new({
        let my_time = Rc::clone(&my_time);
        move || *(*my_time).borrow()
    }));
    let fs = Box::new(PatchedFileSystem(fs, edit.to_patches().await?));
    let mut main = Main::new(Box::new(LoadingScene::new(edit.info, config, fs, None, Some(Rc::new(move || (vw, vh)))).await?), tm, target)?;
    main.show_billboard = false;

    let mut bytes = vec![0; vw as usize * vh as usize * 3];

    const O: f64 = LoadingScene::TOTAL_TIME as f64 + GameScene::BEFORE_TIME as f64;
    const A: f64 = 0.7 + 0.3 + 0.4;

    let fps = v_config.fps;
    let frame_delta = 1. / fps as f32;
    let length = track_length - chart.offset.min(0.) as f64 + 1.;
    let video_length = O + length + A + ending.duration().as_secs_f64();

    let output = Command::new(&ffmpeg).arg("-codecs").output().context("无法执行 ffmpeg")?;
    let codecs = String::from_utf8(output.stdout)?;
    let use_cuda = v_config.hardware_accel && codecs.contains("h264_nvenc");
    let has_qsv = v_config.hardware_accel && codecs.contains("h264_qsv");

    let mut args = "-y -f rawvideo -vcodec rawvideo".to_owned();
    if use_cuda {
        args += " -hwaccel_output_format cuda";
    }
    write!(
        &mut args,
        " -s {vw}x{vh} -r {fps} -pix_fmt rgb24 -i - -c:v {} -qp 0 -vf vflip t_video.mp4",
        if use_cuda {
            "h264_nvenc"
        } else if has_qsv {
            "h264_qsv"
        } else if v_config.hardware_accel {
            bail!("不支持硬件加速！");
        } else {
            "libx264 -preset ultrafast"
        }
    )?;

    let mut proc = Command::new(&ffmpeg)
        .args(args.split_whitespace())
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("无法执行 ffmpeg")?;
    let input = proc.stdin.as_mut().unwrap();

    let offset = chart.offset.max(0.);
    let frames = (video_length / frame_delta as f64).ceil() as u64;
    let start_time = Instant::now();
    for frame in 0..frames {
        *my_time.borrow_mut() = (frame as f32 * frame_delta).max(0.) as f64;
        main.update()?;
        main.render(&mut Ui::new())?;
        gl.flush();

        texture.read_pixels(&mut bytes);
        input.write_all(&bytes)?;
        if frame % 100 == 0 {
            info!("{frame} / {frames}, {:.2}fps", frame as f64 / start_time.elapsed().as_secs_f64());
        }
    }
    proc.wait()?;

    info!("[2] 混音中...");
    let sample_rate = 44100;
    assert_eq!(sample_rate, ending.sample_rate);
    assert_eq!(sample_rate, sfx_click.sample_rate);
    assert_eq!(sample_rate, sfx_drag.sample_rate);
    assert_eq!(sample_rate, sfx_flick.sample_rate);
    let mut output = vec![0.; (video_length * sample_rate as f64).ceil() as usize * 2];
    {
        let pos = O - chart.offset.min(0.) as f64;
        let count = (music.duration().as_secs_f64() * sample_rate as f64) as usize;
        let frames = music.frames.deref();
        let mut it = output[((pos * sample_rate as f64).round() as usize * 2)..].iter_mut();
        let ratio = music.sample_rate as f64 / sample_rate as f64;
        for frame in 0..count {
            let position = (frame as f64 * ratio).round() as usize;
            let frame = frames[position];
            *it.next().unwrap() += frame.left;
            *it.next().unwrap() += frame.right;
        }
    }
    let mut place = |pos: f64, clip: &AudioClip| {
        let position = (pos * sample_rate as f64).round() as usize * 2;
        let mut it = output[position..].iter_mut();
        // TODO optimize?
        for frame in clip.frames.iter() {
            let dst = it.next().unwrap();
            *dst += frame.left;
            let dst = it.next().unwrap();
            *dst += frame.right;
        }
    };
    for note in chart.lines.iter().flat_map(|it| it.notes.iter()).filter(|it| !it.fake) {
        place(
            O + note.time as f64 + offset as f64,
            match note.kind {
                NoteKind::Click | NoteKind::Hold { .. } => &sfx_click,
                NoteKind::Drag => &sfx_drag,
                NoteKind::Flick => &sfx_flick,
            },
        )
    }
    place(O + length + A, &ending);

    info!("[3] 合并 & 压缩…");
    let mut proc = Command::new(ffmpeg)
        .args(
            "-y -i t_video.mp4 -f f32le -ar 44100 -ac 2 -i - -vf format=yuv420p -c:a mp3 -map 0:v:0 -map 1:a:0 out.mp4"
                .to_string()
                .split_whitespace(),
        )
        .stdin(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("无法执行 ffmpeg")?;
    let input = proc.stdin.as_mut().unwrap();
    let mut writer = BufWriter::new(input);
    for sample in output.into_iter() {
        writer.write_all(&sample.to_le_bytes())?;
    }
    drop(writer);
    proc.wait()?;
    std::fs::remove_file("t_video.mp4")?;

    info!("[4] 完成！");

    Ok(())
}
