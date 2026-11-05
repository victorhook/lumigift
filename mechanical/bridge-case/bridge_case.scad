// Case for the nrf24-usb-bridge dongle: a Tenstar "Mini STM32F103C8T6" board
// at the bottom and the nRF24L01+PA+LNA (SMA) module stacked above it.
//
//   USB-C end (x = 0)                                   SMA end
//   ┌──────────────────────────────────────────────────────┐
//   │  ┌──────────── nRF24 PA+LNA, components up ───────────┤══ SMA
//   │  └─────────────────────────────────────────────────── │
//   │    wires                        free: bulk cap, LDO   │
//  ═╪ ┌── STM32 mini ──┐ SWD pins →                         │
//   └──────────────────────────────────────────────────────┘
//
// MEASURE YOUR BOARDS and fix the "Boards" values below before printing;
// the defaults are estimates. Everything else follows from them.
//
// Render:  openscad -D 'part="base"' -o base.stl bridge_case.scad
//          openscad -D 'part="lid"'  -o lid.stl  bridge_case.scad
// Print:   base upright, lid flat side down. 0.2 mm layers, no supports.

part = "assembly";   // "base" | "lid" | "assembly"

/* ---------- Boards (measure!) ---------- */
stm_l      = 28.0;  // STM32 board length, USB edge to SWD-header edge
stm_w      = 18.5;  // STM32 board width
stm_t      = 1.6;   // PCB thickness
stm_under  = 1.0;   // tallest part on the STM32 board's underside (0 if none)
usb_y      = 0;     // USB-C centre offset from the board's centreline (+y = left, seen from USB end)
usb_z      = 1.6;   // USB-C receptacle centre above the PCB's top face (top-mount C ≈ 1.6)

nrf_l      = 41.5;  // nRF module PCB length (without SMA thread)
nrf_w      = 15.6;  // nRF module PCB width
nrf_t      = 1.6;
nrf_top    = 2.6;   // tallest part on the nRF top side (the SMA body sticks up ~2.4 above the PCB)
sma_d      = 6.35;  // SMA thread diameter (1/4")
sma_beyond = 0.0;   // how far the SMA's square body sticks out past the nRF PCB edge

gap        = 4.5;   // STM32 top face to nRF bottom face: USB-C receptacle (3.2) + nRF header solder stubs

/* ---------- Case ---------- */
tol     = 0.2;   // clearance around each board
wall    = 1.2;
end_w   = 1.0;   // thinner end walls: USB plug and SMA thread need reach
floor_t = 0.8;
lid_t   = 0.8;
lip     = 1.5;   // depth of the lid's insert lip
lip_tol = 0.15;  // lid lip clearance
snap_w  = 8.0;   // snap tab width (one tab per long side, mid-length)
snap_t  = 0.8;   // tab thickness: thinner = easier click, weaker hold
snap_l  = 4.5;   // tab length below the lid
snap_b  = 0.35;  // how far the catch sticks out into the wall's window
plug_w  = 12.6;  // USB-C plug overmould opening
plug_h  = 7.0;
r_out   = 1.5;   // outer corner radius
led_x   = -1;    // >= 0: LED window in the +y side wall, x from inner USB wall (-1 = none)
$fn = 48;

/* ---------- Derived ---------- */
assert(nrf_l >= stm_l, "layout assumes the nRF module is longer than the STM32 board");
in_l = nrf_l + sma_beyond + 2 * tol;
in_w = max(stm_w, nrf_w) + 2 * tol;
z_stm  = stm_under;                 // STM32 PCB bottom (inner floor = 0)
z_stmt = z_stm + stm_t;             // STM32 PCB top
z_nrf  = z_stmt + gap;              // nRF PCB bottom
z_nrft = z_nrf + nrf_t;             // nRF PCB top
in_h   = z_nrft + nrf_top + 0.1;
sma_z  = z_nrf + nrf_t / 2;
nrf_x0 = in_l - tol - sma_beyond - nrf_l;   // nRF pushed to the SMA end

out_l = in_l + 2 * end_w;
out_w = in_w + 2 * wall;
out_h = floor_t + in_h;             // base height; lid sits on top

snap_x  = (in_l - snap_w) / 2;
snap_zt = in_h - snap_l;            // tab tip
// catch profile on the y = 0 side, (y, z): gentle lead-in below, 45° lock face above
snap_profile = [[lip_tol, snap_zt + 0.2], [lip_tol - snap_b, snap_zt + 1.2],
                [lip_tol - snap_b, snap_zt + 1.4], [lip_tol, snap_zt + 1.4 + snap_b]];
module both_sides() { children(); translate([0, in_w, 0]) mirror([0, 1, 0]) children(); }

echo(str("Outside: ", out_l, " x ", out_w, " x ", out_h + lid_t, " mm"));

module rbox(l, w, h, r) {
    hull() for (x = [r, l - r], y = [r, w - r]) translate([x, y, 0]) cylinder(r = r, h = h);
}

// Inner-cavity coordinates: origin at the inner floor's USB-end corner.
module inner() { translate([end_w, wall, floor_t]) children(); }

// Ledge along a wall with a 45° chamfer underneath, so it prints without support.
module ledge(len, depth, top_z) {
    translate([0, 0, top_z - depth - 1])
        rotate([90, 0, 90]) linear_extrude(len)
            polygon([[0, 0], [depth, depth], [depth, depth + 1], [0, depth + 1]]);
}

module base() {
    difference() {
        rbox(out_l, out_w, out_h, r_out);
        inner() cube([in_l, in_w, in_h + 1]);
        // USB-C plug opening, centred on the receptacle
        inner() translate([-end_w - 1, in_w / 2 + usb_y, z_stmt + usb_z])
            rotate([0, 90, 0]) hull() for (dy = [-(plug_w - plug_h) / 2, (plug_w - plug_h) / 2])
                translate([0, dy, 0]) cylinder(d = plug_h, h = end_w + 2);
        // SMA slot, open to the top; the lid's tongue closes it
        inner() translate([in_l - 1, in_w / 2, sma_z]) {
            rotate([0, 90, 0]) cylinder(d = sma_d + 0.5, h = end_w + 2);
            translate([0, -(sma_d + 0.5) / 2, 0]) cube([end_w + 2, sma_d + 0.5, in_h]);
        }
        // snap windows: the lid's catches click into these; push a pin in to release
        inner() both_sides() translate([snap_x - 0.2, -wall - 1, snap_zt + 0.1])
            cube([snap_w + 0.4, wall + 1.01, 1.4 + snap_b + 0.05]);
        // optional LED window
        if (led_x >= 0) inner() translate([led_x - 1, in_w - 1, z_stmt])
            cube([2, wall + 2, 2]);
    }
    inner() {
        // STM32 support ledges along both long sides (keep the middle free for underside parts)
        for (y = [0, in_w - tol - 0.8]) translate([0, y, 0]) cube([stm_l + tol, tol + 0.8, z_stm]);
        // STM32 end stop beside the SWD header (corners only)
        for (y = [0, in_w - 2.5]) translate([stm_l + 2 * tol, y, 0]) cube([1.2, 2.5, z_stmt + 1]);

        // nRF rails along the long walls, past the STM32 board (wires live above the STM32)
        nrf_side = (in_w - nrf_w) / 2;
        rail_x0  = stm_l + 2 * tol + 1.2;
        rail_d   = nrf_side + 1.0;
        translate([rail_x0, 0, 0]) ledge(in_l - rail_x0, rail_d, z_nrf);
        translate([in_l, in_w, 0]) rotate([0, 0, 180]) ledge(in_l - rail_x0, rail_d, z_nrf);
        // nRF corner pads on the USB end wall (corners only: the nRF header is mid-width)
        pad_w = nrf_side + 2;
        pad_d = nrf_x0 + 1.5;
        for (y = [0, in_w - pad_w]) translate([0, y + pad_w, 0]) rotate([90, 0, 0])
            linear_extrude(pad_w) polygon([[0, z_nrf - pad_d - 1], [pad_d, z_nrf - 1], [pad_d, z_nrf], [0, z_nrf]]);
    }
}

module lid() {
    // built upside up in place; the "lid" part flips it for printing
    translate([0, 0, out_h]) rbox(out_l, out_w, lid_t, r_out);
    inner() {
        // insert lip (a frame)
        translate([0, 0, in_h - lip]) difference() {
            translate([lip_tol, lip_tol, 0]) cube([in_l - 2 * lip_tol, in_w - 2 * lip_tol, lip]);
            translate([1.2 + lip_tol, 1.2 + lip_tol, -1]) cube([in_l - 2.4 - 2 * lip_tol, in_w - 2.4 - 2 * lip_tol, lip + 2]);
            // slits so the snap tabs flex free of the lip
            both_sides() translate([snap_x - 0.5, -1, -1]) cube([snap_w + 1, 3, lip + 2]);
        }
        // snap tabs with their catches
        both_sides() {
            translate([snap_x, lip_tol, snap_zt]) cube([snap_w, snap_t, snap_l + 0.01]);
            translate([snap_x, 0, 0]) rotate([90, 0, 90]) linear_extrude(snap_w) polygon(snap_profile);
        }
        // hold-down ribs on the nRF's long edges (stop short of the SMA body)
        nrf_side = (in_w - nrf_w) / 2;
        for (y = [nrf_side + 0.2, in_w - nrf_side - 1.0])
            translate([nrf_x0 + 1, y, z_nrft + 0.1]) cube([nrf_l - 9, 0.8, in_h - z_nrft]);
        // tongue closing the SMA slot
        translate([in_l - 0.01, in_w / 2, 0]) difference() {
            translate([0, -(sma_d + 0.5) / 2 + lip_tol, sma_z]) cube([end_w + 0.01, sma_d + 0.5 - 2 * lip_tol, in_h - sma_z]);
            translate([-1, 0, sma_z]) rotate([0, 90, 0]) cylinder(d = sma_d + 0.5, h = end_w + 2);
        }
    }
}

module stm_dummy() inner() {
    color("seagreen") translate([tol, (in_w - stm_w) / 2, z_stm]) cube([stm_l, stm_w, stm_t]);
    color("silver") translate([-0.6, in_w / 2 + usb_y - 4.47, z_stmt]) cube([7.35, 8.94, 3.2]);
}
module nrf_dummy() inner() {
    color("black") translate([nrf_x0, (in_w - nrf_w) / 2, z_nrf]) cube([nrf_l, nrf_w, nrf_t]);
    color("gold") translate([nrf_x0 + nrf_l - 5, in_w / 2, sma_z]) rotate([0, 90, 0]) cylinder(d = sma_d, h = 12);
}

if (part == "base") base();
else if (part == "lid") translate([0, out_w, out_h + lid_t]) rotate([180, 0, 0]) lid();
else { base(); stm_dummy(); nrf_dummy(); %lid(); }
