// I2C test-signal generator for the Hantek MSO5202D serial decoder.
//
// Bit-bangs an I2C master (SCL + SDA) from an ESP-WROOM-32, streaming short
// write transactions of the 0x00..0xFF ramp forever so the scope's I2C decode can
// be verified byte-for-byte. Each transaction: START, address+W, 8 ramp data
// bytes, STOP — so a START appears often (easy to catch in a 3840-sample screen).
//
//   SCL = GPIO13  -> scope CH1   (decoder --scl 0)
//   SDA = GPIO14  -> scope CH2   (decoder --sda 1)
//
// This is a *synthetic* generator: there is no real slave, so the master drives
// the ACK slot low itself (self-ACK) and both lines are push-pull (not the true
// open-drain bus). That produces a clean, textbook START/addr/data/ACK/STOP
// waveform for decode testing — it is not a bus you should hang real devices on.
// Output is 3.3 V CMOS — share GND with the scope. Suggested timebase: 200 us/div.

#include <Arduino.h>

static const uint8_t SCL_PIN = 13;    // -> scope CH1
static const uint8_t SDA_PIN = 14;    // -> scope CH2
static const uint8_t ADDR = 0x50;     // 7-bit slave address (arbitrary)

#ifndef Q_US
#define Q_US 5                        // per-edge dwell (us); SCL ~ 1/(3·Q) MHz
#endif
static uint8_t value = 0;

static inline void scl(int v) { digitalWrite(SCL_PIN, v); delayMicroseconds(Q_US); }
static inline void sda(int v) { digitalWrite(SDA_PIN, v); delayMicroseconds(Q_US); }

static inline void i2cStart() { sda(1); scl(1); sda(0); scl(0); }   // SDA falls while SCL high
static inline void i2cStop()  { sda(0); scl(1); sda(1); }           // SDA rises while SCL high
static inline void i2cBit(int b) { sda(b); scl(1); scl(0); }        // set SDA, pulse SCL

// One byte MSB-first + a self-driven ACK bit (SDA low on the 9th clock).
static inline void i2cByte(uint8_t v) {
    for (int i = 7; i >= 0; i--) i2cBit((v >> i) & 1u);
    i2cBit(0);                                         // ACK (self)
}

void setup() {
    Serial.begin(115200);
    delay(200);
    pinMode(SCL_PIN, OUTPUT);
    pinMode(SDA_PIN, OUTPUT);
    digitalWrite(SCL_PIN, HIGH);                       // idle bus high
    digitalWrite(SDA_PIN, HIGH);
    Serial.println("\n[esp_i2c_gen] MSO5202D I2C test generator");
    Serial.printf("  SCL = GPIO%u  -> scope CH1  (decoder --scl 0)\n", SCL_PIN);
    Serial.printf("  SDA = GPIO%u  -> scope CH2  (decoder --sda 1)\n", SDA_PIN);
    Serial.printf("  addr 0x%02X + ramp 0x00..0xFF, ~%lu kHz, self-ACK (push-pull, no real slave)\n",
                  ADDR, 250UL / Q_US);
    Serial.println("  3.3 V CMOS out — common GND with the scope. Timebase ~200 us/div.");
    Serial.println("  Decode: python3 mso5202d_decode.py decode cap.npz --proto i2c --scl 0 --sda 1");
}

void loop() {
    // Short transaction so a START/STOP pair lands in most captures.
    i2cStart();
    i2cByte((ADDR << 1) | 0);                          // address + write
    for (int k = 0; k < 8; k++) i2cByte(value++);      // 8 ramp data bytes
    i2cStop();
    delayMicroseconds(200);                            // gap between transactions
}
