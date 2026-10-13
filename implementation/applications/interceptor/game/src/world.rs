// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda Bay: the road, the land and the sea, made once at start-up from
//! the layout and a seed. Deterministic: the same world everywhere.

use alloc::vec::Vec;

use crate::city::{self, Block};
use crate::lamps::{self, Lamp};
use crate::layout::VEDA_BAY;
use crate::road::Road;
use crate::scenery::{self, Billboard, Neon, Tree};
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
    /// The trees on the hills and along the coast.
    pub trees: Vec<Tree>,
    /// The billboards along the open road, and the neon signs on roofs.
    pub billboards: Vec<Billboard>,
    pub neons: Vec<Neon>,
}

impl World {
    /// Makes Veda Bay.
    pub fn new() -> World {
        let road = Road::new(VEDA_BAY);
        let terrain = Terrain::new(&road, SEED);
        let blocks = city::buildings(&road, &terrain, SEED);
        let lamps = lamps::place(&road);
        let billboards = scenery::billboards(&road, &terrain);
        let neons = scenery::neons(&road, &blocks);
        let trees = scenery::trees(&road, &terrain, &blocks, &billboards, SEED);
        World { road, terrain, blocks, lamps, trees, billboards, neons }
    }

    /// The period of the lane markings' dashes (m): 12 m, adjusted so that
    /// a whole number of them goes round the loop.
    pub fn dash_period(&self) -> f32 {
        let l = self.road.length();
        l / ((l / 12.0 + 0.5) as u32).max(1) as f32
    }
}
