ENTRY(_start)
MEMORY {
  FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 16K
  RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 4K
}
SECTIONS {
  .text : { *(.text.start) *(.text*) *(.rodata*) . = ALIGN(4); } > FLASH
  .data : {
    __data_start = .;
    *(.data*) *(.sdata*)
    . = ALIGN(4); __data_end = .;
  } > RAM AT>FLASH
  __data_load = LOADADDR(.data);
  .bss (NOLOAD) : {
    __bss_start = .;
    *(.bss*) *(.sbss*) *(COMMON)
    . = ALIGN(4); __bss_end = .;
  } > RAM
}
