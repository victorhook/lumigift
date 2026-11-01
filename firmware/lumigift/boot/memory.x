/* Lumigift bootloader: pages 0..=4 of the STM32C011F6 (10 KiB,
 * lumigift_proto::layout::BOOT_SIZE). The last 16 bytes hold the boot info
 * block read by the app. The last 16 bytes of RAM are BootShared and must
 * not be used here. RAM functions (.data.ramfunc.*) are copied to RAM with
 * .data by cortex-m-rt's startup code. */
MEMORY
{
  FLASH    : ORIGIN = 0x08000000, LENGTH = 10K - 16
  BOOTINFO : ORIGIN = 0x080027F0, LENGTH = 16
  RAM      : ORIGIN = 0x20000000, LENGTH = 6K - 16
}

/* Lowest flash address this image may erase/program (board::flash write
 * guard): the app region and the config page. */
__lumigift_flash_floor = 0x08002800;

SECTIONS
{
  .bootinfo ORIGIN(BOOTINFO) :
  {
    KEEP(*(.bootinfo .bootinfo.*));
  } > BOOTINFO
} INSERT AFTER .uninit;
