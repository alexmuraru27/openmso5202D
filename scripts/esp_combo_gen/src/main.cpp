// Combined analog + logic-analyzer test generator for the MSO5202D.
//
// Drives BOTH at once so you can capture CH1/CH2 (analog) and all 16 LA channels
// in one acquisition:
//   * a selectable serial protocol on CH1/CH2 (for the analog serial decoders), and
//   * the 16 LA channels with a distinct frequency per line (f_N = 1000/(N+1) Hz)
//     so the logic pod has known, per-channel-identifiable inputs.
//
// Pinning (chosen so nothing overlaps):
//   CH1 (analog) = GPIO22      CH2 (analog) = GPIO23      <- clip the scope probes here
//   LA D0..D15   = the 16 pins below (frees GPIO13/14 for the LA, unlike the
//                  single-protocol sketches which put the serial line there).
//
// Serial protocol via -DPROTO (0=SPI default, 1=UART, 2=I2C):
//   SPI : SCLK=CH1(22)  MOSI=CH2(23)   mode 0, MSB, ~20 kHz, ramp 0x00..0xFF
//   UART: TX=CH1(22)    (CH2 unused)   8N1, 9600 baud, ramp
//   I2C : SCL=CH1(22)   SDA=CH2(23)    self-ACK ~50 kHz, ramp
// 3.3 V CMOS out — common GND with the scope. See README.md.

#include <Arduino.h>

// ---------------- LA: 16 channels, distinct frequency each ----------------
// Index i = LA channel Di. f_N = 1000/(N+1) Hz (D0 = 1 kHz … D15 = 62.5 Hz).
static const uint8_t LA_PIN[16] = {
    13, 12, 14, 27, 26, 25, 33, 32, 15, 2, 4, 16, 17, 5, 18, 19,
};
static const uint32_t LA_HALF_US_BASE = 500;      // half-period of Dn = 500us*(n+1)
static uint32_t laNext[16];
static uint8_t  laState[16];

static inline void laInit(uint32_t now) {
    for (int i = 0; i < 16; i++) {
        pinMode(LA_PIN[i], OUTPUT);
        laState[i] = 0;
        digitalWrite(LA_PIN[i], LOW);
        laNext[i] = now + LA_HALF_US_BASE * (uint32_t)(i + 1);
    }
}
// Non-blocking: flip any Dn whose half-period elapsed. Cheap (16 compares).
static inline void laTick(uint32_t now) {
    for (int i = 0; i < 16; i++) {
        if ((int32_t)(now - laNext[i]) >= 0) {
            laState[i] ^= 1u;
            digitalWrite(LA_PIN[i], laState[i]);
            laNext[i] += LA_HALF_US_BASE * (uint32_t)(i + 1);
        }
    }
}

// ---------------- serial on CH1/CH2 ----------------
#define P_SPI  0
#define P_UART 1
#define P_I2C  2
#ifndef PROTO
#define PROTO P_SPI
#endif

static const uint8_t CH1 = 22;    // scope CH1
static const uint8_t CH2 = 23;    // scope CH2
static uint8_t value = 0;
static uint32_t nextSerial = 0;

#if PROTO == P_SPI
#include <SPI.h>
static const uint32_t SPI_HZ = 20000;
static const uint32_t SERIAL_GAP_US = 300;        // idle-clock gap -> decoder reframes
SPIClass spi(VSPI);
static void serialInit() { spi.begin(CH1, -1, CH2, -1); }   // SCK=22, MISO none, MOSI=23
static void serialUnit() {
    spi.beginTransaction(SPISettings(SPI_HZ, MSBFIRST, SPI_MODE0));
    spi.transfer(value++);
    spi.endTransaction();
}
#elif PROTO == P_UART
static const uint32_t BAUD = 9600;
static const uint32_t SERIAL_GAP_US = 3 * 1000000UL / BAUD;
HardwareSerial uart(1);
static void serialInit() { uart.begin(BAUD, SERIAL_8N1, -1, CH1); }   // TX=22
static void serialUnit() { uart.write(value++); uart.flush(); }
#else  // P_I2C — bit-banged self-ACK master
static const uint32_t Q_US = 5;                    // ~50-60 kHz SCL
static const uint32_t SERIAL_GAP_US = 400;         // gap between transactions
static const uint8_t ADDR = 0x50;
static inline void scl(int v) { digitalWrite(CH1, v); delayMicroseconds(Q_US); }
static inline void sda(int v) { digitalWrite(CH2, v); delayMicroseconds(Q_US); }
static inline void i2cBit(int b) { sda(b); scl(1); scl(0); }
static inline void i2cByte(uint8_t v) { for (int i = 7; i >= 0; i--) i2cBit((v >> i) & 1u); i2cBit(0); }
static void serialInit() { pinMode(CH1, OUTPUT); pinMode(CH2, OUTPUT); digitalWrite(CH1, HIGH); digitalWrite(CH2, HIGH); }
static void serialUnit() {
    sda(1); scl(1); sda(0); scl(0);                // START
    i2cByte((ADDR << 1) | 0);
    for (int k = 0; k < 4; k++) i2cByte(value++);  // 4 ramp bytes / transaction
    sda(0); scl(1); sda(1);                        // STOP
}
#endif

void setup() {
    Serial.begin(115200);
    delay(200);
    const uint32_t now = micros();
    laInit(now);
    serialInit();
    nextSerial = now + SERIAL_GAP_US;
    Serial.println("\n[esp_combo_gen] MSO5202D combined analog + LA generator");
#if PROTO == P_SPI
    Serial.println("  serial: SPI  SCLK=GPIO22(CH1) MOSI=GPIO23(CH2), mode0/MSB ~20kHz");
#elif PROTO == P_UART
    Serial.println("  serial: UART TX=GPIO22(CH1), 8N1 9600 baud (CH2 unused)");
#else
    Serial.println("  serial: I2C  SCL=GPIO22(CH1) SDA=GPIO23(CH2), self-ACK ~50kHz");
#endif
    Serial.println("  LA D0..D15 on 13,12,14,27,26,25,33,32,15,2,4,16,17,5,18,19");
    Serial.println("  f_N = 1000/(N+1) Hz (D0=1kHz..D15=62.5Hz). 3.3V CMOS, common GND.");
    Serial.println("  ramp 0x00..0xFF on CH1/CH2; LA threshold ~1.5V.");
}

void loop() {
    const uint32_t now = micros();
    laTick(now);                                   // keep all 16 LA lines toggling
    if ((int32_t)(now - nextSerial) >= 0) {        // pace the serial, non-blocking
        serialUnit();
        nextSerial = micros() + SERIAL_GAP_US;
    }
}
