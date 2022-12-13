use super::{Matrix, Point, JUDGE_LINE_PERFECT_COLOR};
use crate::{
    audio::{Audio, AudioClip, DefaultAudio, PlayParams},
    config::Config,
    fs::FileSystem,
    info::ChartInfo,
    particle::{AtlasConfig, ColorCurve, Emitter, EmitterConfig},
};
use anyhow::{Context, Result};
use image::imageops::blur;
use macroquad::prelude::*;

const FONT_PATH: &str = "font.ttf";

pub struct NoteStyle {
    pub click: Texture2D,
    pub hold_head: Texture2D,
    pub hold: Texture2D,
    pub hold_tail: Texture2D,
    pub flick: Texture2D,
    pub drag: Texture2D,
}

pub struct Resource {
    pub config: Config,
    pub info: ChartInfo,
    pub aspect_ratio: f32,
    pub last_screen_size: (u32, u32),

    pub time: f32,

    pub judge_line_color: Color,

    pub camera: Camera2D,
    pub camera_matrix: Mat4,

    pub font: Font,
    pub background: Texture2D,
    pub note_style: NoteStyle,
    pub note_style_mh: NoteStyle,
    pub icon_back: Texture2D,
    pub icon_retry: Texture2D,
    pub icon_resume: Texture2D,

    pub emitter: Emitter,
    pub emitter_square: Emitter,

    pub audio: DefaultAudio,
    pub music: AudioClip,
    pub track_length: f32,
    pub sfx_click: AudioClip,
    pub sfx_drag: AudioClip,
    pub sfx_flick: AudioClip,

    pub model_stack: Vec<Matrix>,
}

impl Resource {
    pub async fn new(config: Config, info: ChartInfo, mut fs: Box<dyn FileSystem>) -> Result<Self> {
        macro_rules! load_tex {
            ($path:literal) => {
                Texture2D::from_image(&load_image($path).await?)
            };
        }
        let hold_tail = load_tex!("hold_tail.png");
        let note_style = NoteStyle {
            click: load_tex!("click.png"),
            hold_head: load_tex!("hold_head.png"),
            hold: load_tex!("hold.png"),
            hold_tail,
            flick: load_tex!("flick.png"),
            drag: load_tex!("drag.png"),
        };
        let camera = Camera2D {
            target: vec2(0., 0.),
            zoom: vec2(1., config.aspect_ratio.unwrap_or(info.aspect_ratio)),
            ..Default::default()
        };
        let colors_curve = {
            let start = WHITE;
            let mut mid = start;
            let mut end = start;
            mid.a *= 0.7;
            end.a = 0.;
            ColorCurve { start, mid, end }
        };

        async fn load_background(fs: &mut Box<dyn FileSystem>, path: &str) -> Result<Texture2D> {
            let image = image::load_from_memory(&fs.load_file(path).await?)
                .context("Failed to decode image")?;
            let image = blur(&image, 15.);
            Ok(Texture2D::from_image(&Image {
                width: image.width() as u16,
                height: image.height() as u16,
                bytes: image.into_raw(),
            }))
        }

        let background = match load_background(&mut fs, &info.illustration).await {
            Ok(bg) => Some(bg),
            Err(err) => {
                warn!("Failed to load background: {:?}", err);
                None
            }
        };
        let background = background.unwrap_or_else(|| Texture2D::from_rgba8(1, 1, &[0, 0, 0, 1]));

        let audio = DefaultAudio::new()?;
        macro_rules! load_sfx {
            ($path:literal) => {
                audio.create_clip(load_file($path).await?)?.0
            };
        }
        let (music, track_length) = audio.create_clip(fs.load_file(&info.music).await?)?;
        let track_length = track_length as f32;
        let sfx_click = load_sfx!("click.ogg");
        let sfx_drag = load_sfx!("drag.ogg");
        let sfx_flick = load_sfx!("flick.ogg");

        let aspect_ratio = config.aspect_ratio.unwrap_or(info.aspect_ratio);
        Ok(Self {
            config,
            info,
            aspect_ratio,
            last_screen_size: (0, 0),

            time: 0.0,

            judge_line_color: JUDGE_LINE_PERFECT_COLOR,

            camera,
            camera_matrix: camera.matrix(),

            background,
            font: match load_ttf_font(FONT_PATH).await {
                Err(err) => {
                    warn!("Failed to load font from {FONT_PATH}, falling back to default\n{err:?}");
                    Font::default()
                }
                Ok(font) => font,
            },
            note_style,
            note_style_mh: NoteStyle {
                click: load_tex!("click_mh.png"),
                hold_head: load_tex!("hold_head_mh.png"),
                hold: load_tex!("hold_mh.png"),
                hold_tail,
                flick: load_tex!("flick_mh.png"),
                drag: load_tex!("drag_mh.png"),
            },
            icon_back: load_tex!("back.png"),
            icon_retry: load_tex!("retry.png"),
            icon_resume: load_tex!("resume.png"),

            emitter: Emitter::new(EmitterConfig {
                local_coords: false,
                texture: Some(load_tex!("hit_fx.png")),
                lifetime: 0.5,
                lifetime_randomness: 0.0,
                initial_direction_spread: 0.0,
                initial_velocity: 0.0,
                size: 1. / 5.,
                atlas: Some(AtlasConfig::new(5, 6, ..)),
                emitting: false,
                colors_curve,
                ..Default::default()
            }),
            emitter_square: Emitter::new(EmitterConfig {
                local_coords: false,
                lifetime: 0.5,
                lifetime_randomness: 0.0,
                initial_direction_spread: 2. * std::f32::consts::PI,
                size: 1. / 65.,
                emitting: false,
                initial_velocity: 1.4,
                initial_velocity_randomness: 1. / 10.,
                linear_accel: -4. / 1.,
                colors_curve,
                ..Default::default()
            }),

            audio,
            music,
            track_length,
            sfx_click,
            sfx_drag,
            sfx_flick,

            model_stack: vec![Matrix::identity()],
        })
    }

    pub fn emit_at_origin(&mut self, color: Color) {
        if !self.config.particle {
            return;
        }
        let pt = self.world_to_screen(Point::default());
        let pt = vec2(pt.x, pt.y);
        self.emitter.config.base_color = color;
        self.emitter.emit(pt, 1);
        self.emitter_square.config.base_color = color;
        self.emitter_square.emit(pt, 4);
    }

    pub fn update_size(&mut self, dim: (u32, u32)) -> bool {
        if self.last_screen_size == dim {
            return false;
        }
        fn viewport(aspect_ratio: f32, (w, h): (u32, u32)) -> (i32, i32, i32, i32) {
            let w = w as f32;
            let h = h as f32;
            let (rw, rh) = {
                let ew = h * aspect_ratio;
                if ew > w {
                    let eh = w / aspect_ratio;
                    (w, eh)
                } else {
                    (ew, h)
                }
            };
            (
                ((w - rw) / 2.).round() as i32,
                ((h - rh) / 2.).round() as i32,
                rw as i32,
                rh as i32,
            )
        }
        let aspect_ratio = self.config.aspect_ratio.unwrap_or(self.info.aspect_ratio);
        if self.config.fix_aspect_ratio {
            self.aspect_ratio = aspect_ratio;
            self.camera.viewport = Some((0, 0, dim.0 as _, dim.1 as _));
        } else {
            self.aspect_ratio = aspect_ratio.min(dim.0 as f32 / dim.1 as f32);
            self.camera.zoom = vec2(1., self.aspect_ratio);
            self.camera.viewport = Some(viewport(self.aspect_ratio, dim));
        };
        true
    }

    pub fn play_sfx(&mut self, sfx: &AudioClip) {
        if self.config.volume_sfx <= 1e-2 {
            return;
        }
        let _ = self.audio.play(
            sfx,
            PlayParams {
                volume: self.config.volume_sfx,
                ..Default::default()
            },
        );
    }

    pub fn world_to_screen(&self, pt: Point) -> Point {
        self.model_stack.last().unwrap().transform_point(&pt)
    }

    pub fn screen_to_world(&self, pt: Point) -> Point {
        self.model_stack
            .last()
            .unwrap()
            .try_inverse()
            .unwrap()
            .transform_point(&pt)
    }

    #[inline]
    pub fn with_model(&mut self, model: Matrix, f: impl FnOnce(&mut Self)) {
        let model = self.model_stack.last().unwrap() * model;
        self.model_stack.push(model);
        f(self);
        self.model_stack.pop();
    }

    #[inline]
    pub fn apply_model(&self, f: impl FnOnce()) {
        self.apply_model_of(self.model_stack.last().unwrap(), f);
    }

    #[inline]
    pub fn apply_model_of(&self, mat: &Matrix, f: impl FnOnce()) {
        unsafe { get_internal_gl() }.quad_gl.push_model_matrix({
            /*
                [11] [12]  0  [13]
                [21] [22]  0  [23]
                  0    0   1    0
                [31] [32]  0  [33]
            */
            Mat4::from_cols_array(&[
                mat.m11, mat.m21, 0., mat.m31, mat.m12, mat.m22, 0., mat.m32, 0., 0., 1., 0.,
                mat.m13, mat.m23, 0., mat.m33,
            ])
        });
        f();
        unsafe { get_internal_gl() }.quad_gl.pop_model_matrix();
    }
}