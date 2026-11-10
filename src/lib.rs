pub mod codec;
pub mod forward;
pub mod image_io;
pub mod plus;
pub mod reverse;
pub mod stage;
pub mod tween;
pub mod workflow;

use serde::{Deserialize, Serialize};

pub const MISSING: f32 = -10000.0;
pub const DEFAULTS: [f32; 8] = [0., 0., 0., 0., 1., 1., 0., 1.];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    // x, y, kx, ky, sx, sy, frame, alpha. Raw compiled values may be MISSING.
    pub values: [f32; 8],
    pub image: String,
    pub font: String,
    pub text: String,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            values: DEFAULTS,
            image: String::new(),
            font: String::new(),
            text: String::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub transforms: Vec<Transform>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub fps: f32,
    pub tracks: Vec<Track>,
}
impl Definition {
    pub fn frame_count(&self) -> usize {
        self.tracks
            .iter()
            .map(|t| t.transforms.len())
            .max()
            .unwrap_or(0)
    }
    pub fn expanded(&self) -> Self {
        let mut result = self.clone();
        for track in &mut result.tracks {
            let mut previous = Transform::default();
            for frame in &mut track.transforms {
                for (v, prev) in frame.values.iter_mut().zip(&mut previous.values) {
                    if *v == MISSING {
                        *v = *prev;
                    } else {
                        *prev = *v;
                    }
                }
                for (value, prev) in [
                    (&mut frame.image, &mut previous.image),
                    (&mut frame.font, &mut previous.font),
                    (&mut frame.text, &mut previous.text),
                ] {
                    if value.is_empty() {
                        value.clone_from(prev);
                    } else {
                        prev.clone_from(value);
                    }
                }
            }
        }
        result
    }
}
