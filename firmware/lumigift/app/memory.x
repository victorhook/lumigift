/* Lumigift app: linked behind the 256-byte ImageHeader in the app region.
 * 0x0800_2900 = layout::APP_VECTORS, 20 KiB - 256 B = layout::APP_MAX_SIZE.
 * The last 16 bytes of RAM are BootShared and must not be used here. */
MEMORY
{
  FLASH : ORIGIN = 0x08002900, LENGTH = 20K - 256
  RAM   : ORIGIN = 0x20000000, LENGTH = 6K - 16
}

/* Lowest flash address this image may erase/program (board::flash write
 * guard): the config page only. */
__lumigift_flash_floor = 0x08007800;
