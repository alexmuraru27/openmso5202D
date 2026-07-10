// SPI test-signal generator for the Hantek MSO5202D serial decoder.
//
// Drives an SPI master (SCLK + MOSI) from an ESP-WROOM-32 using the HARDWARE SPI
// peripheral (exact clock at any frequency), streaming the 0x00..0xFF ramp
// forever so the scope's SPI decode can be verified byte-for-byte (any slip shows
// up instantly as a break in the count). MSB-first; clock rate via -DSPI_HZ,
// mode via -DSPI_MODE (0..3, default mode 0).
//
//   SCLK = GPIO13  -> scope CH1   (decoder channel 0)
//   MOSI = GPIO14  -> scope CH2   (decoder channel 1)
//
// There is no chip-select line (we only have 2 analog channels), so a short idle
// gap is left between bytes; the decoder re-frames on that gap, staying aligned
// even when a capture starts mid-byte. Output is 3.3 V CMOS — share GND with the
// scope. Pick a timebase so a clock period spans ≥~8 samples (e.g. 500 us/div at
// 20 kHz; scale down as you raise SPI_HZ).

#include <Arduino.h>
#include <SPI.h>

static const uint8_t SCLK_PIN = 13;   // -> scope CH1
static const uint8_t MOSI_PIN = 14;   // -> scope CH2

#ifndef SPI_HZ
#define SPI_HZ 20000                  // SCLK frequency
#endif
#ifndef SPI_MODE
#define SPI_MODE 0                    // 0..3 (CPOL/CPHA); must match decoder
#endif

static const uint8_t MODE[4] = {SPI_MODE0, SPI_MODE1, SPI_MODE2, SPI_MODE3};
SPIClass spi(VSPI);
static uint8_t value = 0;
// Idle-clock gap between bytes = ~4 clock periods (so the decoder's gap re-framing
// fires), floored at 5 us; scales down as SPI_HZ rises to keep byte density high.
static const uint32_t BYTE_GAP_US =
    (4000000UL / SPI_HZ) > 5 ? (4000000UL / SPI_HZ) : 5;

void setup() {
    Serial.begin(115200);
    delay(200);
    spi.begin(SCLK_PIN, -1, MOSI_PIN, -1);     // SCK, MISO(unused), MOSI, SS(unused)
    Serial.println("\n[esp_spi_gen] MSO5202D SPI test generator");
    Serial.printf("  SCLK = GPIO%u  -> scope CH1  (decoder --clk 0)\n", SCLK_PIN);
    Serial.printf("  MOSI = GPIO%u  -> scope CH2  (decoder --data 1)\n", MOSI_PIN);
    Serial.printf("  mode %d, MSB-first, %lu Hz SCLK (hardware SPI), ramp 0x00..0xFF\n",
                  SPI_MODE, (unsigned long)SPI_HZ);
    Serial.println("  3.3 V CMOS out — common GND with the scope.");
    Serial.println("  Decode: python3 mso5202d_decode.py decode cap.npz --proto spi --clk 0 --data 1"
                   " --cpol <0/1> --cpha <0/1>");
}

void loop() {
    spi.beginTransaction(SPISettings(SPI_HZ, MSBFIRST, MODE[SPI_MODE & 3]));
    spi.transfer(value++);
    spi.endTransaction();
    delayMicroseconds(BYTE_GAP_US);            // idle-clock gap so the decoder re-frames
}
