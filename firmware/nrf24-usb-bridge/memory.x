/* The app sits after the 16 KiB USB DFU bootloader (boot/, APP_BASE in
   boot/src/layout.rs). */
MEMORY
{
  FLASH : ORIGIN = 0x08004000, LENGTH = 48K
  RAM   : ORIGIN = 0x20000000, LENGTH = 20K
}
