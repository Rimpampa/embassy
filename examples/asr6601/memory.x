MEMORY
{
  /* ASR6601 main flash. Some variants expose 256 KiB; adjust for your part. */
  FLASH : ORIGIN = 0x08000000, LENGTH = 128K
  RAM   : ORIGIN = 0x20000000, LENGTH = 16K
}
