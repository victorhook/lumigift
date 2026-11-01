# lumigift-stage

A real-time 3D "stage" of glowing Lumigift boxes for egui (eframe 0.37, wgpu backend).
Each gift is drawn as a frosted white PLA box lit from inside by its four LEDs, standing
on a dark glossy floor with soft coloured light pools, reflections, bloom and filmic
tonemapping. It follows `docs/UI_DESIGN.md` §5.

## Usage

```rust
use lumigift_stage::{Arrangement, Rgb, Stage, StageGift, StageOptions};

// In the eframe creation closure (wgpu backend required):
let mut stage = Stage::new(cc.wgpu_render_state.as_ref().unwrap());

// Every frame, inside some Ui (the stage fills the available space):
let gifts: Vec<StageGift> = /* id, position [x, z] in metres, yaw, 4 LED colours, online, selected, label */;
let r = stage.show(ui, &gifts, &StageOptions::default());
if let Some(id) = r.clicked { /* select */ }
for (id, pos) in r.moved { /* store the new floor position (edit_layout mode) */ }

// Helpers:
stage.frame_all(&gifts);                                       // fit camera to all boxes (animated)
let layout = Stage::auto_layout(&gifts, Arrangement::Arc);     // Vec<(id, [x, z])>
let with_yaw = Stage::auto_layout_with_yaw(&gifts, Arrangement::Circle);
stage.set_preset(lumigift_stage::CameraPreset::Top);
let cam = stage.camera();          // CameraState: serde, persist it
stage.set_camera(cam);             // restore (disables the automatic first-frame framing)
```

* `StageGift::leds` are the bytes sent to the LEDs (pre-gamma sRGB). The stage converts
  them to linear light (`srgb_to_linear(c / 255) × 6`) so saturated LEDs land in the HDR
  range and bloom, while dim values stay dim like the real hardware.
* LED order in box space: 0:(−x,−z) 1:(+x,−z) 2:(+x,+z) 3:(−x,+z). Yaw rotates the box.
* A box is ~6 × 6 × 5.8 cm (`BOX_SIZE` = 0.06 m body width). Arrangement spacing is 15 cm.
* If your `NativeOptions` set `multisampling` or `depth_buffer`, create the stage with
  `Stage::with_target(rs, TargetInfo { msaa_samples, depth_format })` so the composite
  pipeline matches egui's render pass.
* The first `show` with gifts frames them automatically (unless `set_camera` restored a pose).
  A `frame_all` fit follows widget resizes until the user moves the camera.
* Several `Stage`s may exist at once (e.g. studio preview + performance viewport); each owns
  its offscreen targets, which are freed when the `Stage` is dropped.
* The stage requests repaints only while the camera moves, while dragging, and in
  performance mode. The app requests repaints when LED data changes.

### Controls

| Input | Action |
|---|---|
| LMB drag | orbit (pitch clamped 6°…85°, so the camera stays above the floor) |
| RMB / MMB drag, Shift + LMB drag | pan |
| Wheel / pinch | dolly towards the pointer (damped) |
| Double-click | on a box: focus it; elsewhere: frame all |
| Click | `StageResponse::clicked` |
| LMB drag on a box with `edit_layout` | move it on the floor, **snapping to 5 cm**; hold **Shift** for free placement |

`performance: true` hides labels, hints and highlights and orbits the camera at 2°/s.

## Render pipeline

All passes are recorded in the paint callback's `prepare` into egui's encoder, at the
widget's pixel size (`rect × pixels_per_point`; targets are recreated on resize and
nothing is drawn for a zero-sized or hidden widget).

1. **Scene** (`Rgba16Float`, 4× MSAA + `Depth32Float`, resolved to a single-sample HDR
   texture, cleared to `STAGE_BG`):
   * **Reflections**: the instanced boxes again, mirrored in y (flipped winding), with
     dimmed emission and an exponential fade with depth below the floor.
   * **Floor**: a large quad, alpha 0.86 over the reflections (glossy look). Per pixel it
     loops over up to 64 gifts (uniform array) and adds a **light pool**: a tight core plus a
     broad halo around the box footprint, tinted per quadrant by the nearest LED and
     blending to the LED mean further out, with a contact darkening at the base. Selected
     and hovered boxes get an accent ring. Optional anti-aliased grid (10 cm / 50 cm),
     fading out with distance.
   * **Boxes**: one instanced draw (~2.5 k triangles per box, 96-byte instances carrying
     position, yaw, 4 linear LED colours and flags). The shell is opaque; translucency is
     faked with emission: a per-fragment sum of the four LEDs by distance (soft hot-spots on
     the nearest faces), a diffuse scatter term from the LED mean, a dark band under the lid,
     a fresnel boost at grazing angles, plus a dim key light, sky fill and rim so unlit or
     offline boxes still read as white boxes. Selected/hovered boxes get an accent rim.
   * Exponential distance fog towards `STAGE_BG`.
2. **Bloom** ("dual filter" / Call of Duty: Advanced Warfare style): up to 6 half-res mips.
   13-tap downsample with a soft-knee threshold and Karis average on the first step, then a
   3×3 tent upsample additively blended back up the chain.
3. **Composite** (inside egui's render pass, fullscreen triangle with the widget viewport):
   `(scene + bloom × strength) × exposure` → hue-preserving tonemap with a shoulder that
   desaturates very bright light towards white (like the hot LED cores in the photos) →
   vignette → sRGB encode (unless the target is `*Srgb`) → triangular dither to kill
   banding in the dark gradients.

Labels, hints and cursors are painted with the egui painter on top, by projecting 3D anchor
points with the same view-projection matrix.

## Performance

Measured on an Intel Meteor Lake iGPU (Mesa), release build, vsync off: 30 boxes at
1600 × 1000 (performance mode, full window) ≈ 105 fps; 10 boxes ≈ 144 fps. The cost is
dominated by the fragment work at full resolution (MSAA scene + per-pixel pool loop + bloom).

## Demo

```sh
CARGO_TARGET_DIR=target/agent-stage cargo run -p lumigift-stage --example demo
```

Ten boxes in an arc with animated patterns (hue chase, beat pulse, sparkle, Christmas), a
side panel for every `StageOptions` field, arrangements and camera presets. `P` toggles
performance mode. Environment knobs for automated runs are listed at the top of
`examples/demo.rs` (`STAGE_DEMO_GIFTS=30`, `STAGE_DEMO_FPS=1`, …).
