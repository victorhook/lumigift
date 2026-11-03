/* USB DFU bootloader: the first 16 KiB of flash. The app starts right after
   it (APP_BASE in src/layout.rs and the app's memory.x must agree). */
MEMORY
{
  FLASH : ORIGIN = 0x08000000, LENGTH = 16K
  RAM   : ORIGIN = 0x20000000, LENGTH = 20K
}
