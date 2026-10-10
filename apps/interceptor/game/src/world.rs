//! Veda Bay: the road, the land and the sea, made once at start-up from
//! the layout and a seed. Deterministic: the same world everywhere.

use alloc::vec::Vec;

use crate::city::{self, Block};
use crate::lamps::{self, Lamp};
use crate::layout::VEDA_BAY;
use crate::road::Road;
use crate::terrain::Terrain;

/// The seed of everything random in the world.
pub const SEED: u64 = 0x05ED_ABA4;

/// The world.
pub struct World {
    pub road: Road,
    pub terrain: Terrain,
    /// The buildings' blocks.
    pub blocks: Vec<Block>,
    /// The street lights.
    pub lamps: Vec<Lamp>,
}

impl World {
    /// Makes Veda Bay.
    pub fn new() -> World {
        let road = Road::new(VEDA_BAY);
        let terrain = Terrain::new(&road, SEED);
        let blocks = city::buildings(&road, &terrain, SEED);
        let lamps = lamps::place(&road);
        World { road, terrain, blocks, lamps }
    }

    /// The period of the lane markings' dashes (m): 12 m, adjusted so that
    /// a whole number of them goes round the loop.
    pub fn dash_period(&self) -> f32 {
        let l = self.road.length();
        l / ((l / 12.0 + 0.5) as u32).max(1) as f32
    }
}
