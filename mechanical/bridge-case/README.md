# Bridge case

A small two-part case for the bridge dongle (43.9 × 21.3 × 13.0 mm with the default
dimensions). The STM32 mini board lies at the bottom and the nRF24L01+PA+LNA module
sits above it with its components facing up. The USB-C port is at one end and the SMA
connector comes out of the other.

1. Measure both boards and update the `Boards` block in `bridge_case.scad`. The
   defaults are estimates.
2. `openscad -D 'part="base"' -o base.stl bridge_case.scad` (and the same with `"lid"`).
3. Print with PETG or PLA at 0.2 mm, no supports. Print the base upright and the lid
   flat side down.

Assembly: put the STM32 board in first, USB end first, so it rests on the side ledges
against the end stops. A dot of hot glue at the far corners keeps it from rattling.
Route the wires up, lay the nRF module on its rails with the SMA in the slot, and press
the lid on until it clicks: a tab on each long side catches in a window in the wall. To open it, push the catches in through the windows with a pin and lift the lid. The free space under the nRF at the SMA end is meant for the
bulk capacitor or a separate 3.3 V LDO. The SWD pins can stay on the board, because
they point into that same space.
