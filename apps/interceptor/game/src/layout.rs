// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Veda Bay: the loop's control points, from downtown round the bay and
//! back.
//!
//! The road starts on the downtown avenue heading east, sweeps south up
//! into the hills, dives through the headland in a tunnel, follows the
//! cliffs west above the sea, crosses the harbour on the bridge, runs
//! north through the container port and turns east into downtown again.
//! North is -z, east +x; heights are metres above the sea.

use crate::road::ControlPoint;
use crate::road::Section::{self, Bridge, City, Coast, Hills, Port, Tunnel};

const fn p(x: f32, z: f32, y: f32, section: Section) -> ControlPoint {
    ControlPoint { x, z, y, section }
}

/// The loop, in driving order.
pub const VEDA_BAY: &[ControlPoint] = &[
    // Downtown: the avenue east, a block south at the plaza, east again.
    p(-760.0, -700.0, 9.0, City),
    p(-430.0, -712.0, 9.0, City),
    p(-200.0, -706.0, 9.0, City),
    p(-110.0, -680.0, 9.0, City),
    p(-80.0, -600.0, 9.0, City),
    p(-30.0, -548.0, 9.0, City),
    p(160.0, -540.0, 10.0, City),
    p(480.0, -556.0, 11.0, City),
    // The sweep south out of downtown.
    p(780.0, -560.0, 14.0, Hills),
    p(980.0, -450.0, 20.0, Hills),
    // Up through the hills in S-bends.
    p(1080.0, -270.0, 30.0, Hills),
    p(990.0, -100.0, 40.0, Hills),
    p(1110.0, 70.0, 50.0, Hills),
    p(1040.0, 250.0, 56.0, Hills),
    // Through the headland.
    p(1050.0, 420.0, 56.0, Tunnel),
    p(1000.0, 560.0, 54.0, Tunnel),
    p(900.0, 690.0, 52.0, Tunnel),
    // Along the cliffs, west and down.
    p(760.0, 790.0, 48.0, Coast),
    p(560.0, 840.0, 42.0, Coast),
    p(360.0, 800.0, 36.0, Coast),
    p(150.0, 860.0, 30.0, Coast),
    p(-60.0, 830.0, 26.0, Coast),
    // Over the harbour's mouth.
    p(-260.0, 760.0, 26.0, Bridge),
    p(-420.0, 560.0, 28.0, Bridge),
    p(-560.0, 360.0, 22.0, Bridge),
    // North through the port.
    p(-720.0, 150.0, 8.0, Port),
    p(-880.0, -40.0, 5.0, Port),
    p(-970.0, -250.0, 5.0, Port),
    // And east into downtown.
    p(-1000.0, -470.0, 7.0, City),
    p(-920.0, -640.0, 9.0, City),
];
