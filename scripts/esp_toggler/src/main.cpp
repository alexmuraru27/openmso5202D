// LA test-signal generator for the Hantek MSO5202D logic analyzer.
//
// Drives all 16 LA channels (L00..L15) from an ESP-WROOM-32 so each physical
// line carries a distinguishable signal — used to verify the LA channel mapping
// and decode when reverse-engineering the scope's LA menus.
//
// Two patterns (selected at build time via -DPATTERN, default FREQ):
//   FREQ    : each channel is an independent square wave, f_N = 1000/(N+1) Hz.
//             L00 = 1000 Hz (fastest) ... L15 = 62.5 Hz (slowest). 16:1 span,
//             so all 16 are visible together; identify a channel by its rate.
//   COUNTER : a free-running 16-bit binary counter, channel N = counter bit N
//             (L00 toggles fastest, L15 slowest). Lets you read the count and
//             instantly spot a swapped/dead channel — but a very wide freq span.
//
// Output is 3.3 V CMOS. Set the scope's LA threshold to a normal TTL/CMOS level
// (~1.4–1.6 V) so both rails are seen cleanly.

#include <Arduino.h>

// ---- L00..L15  ->  ESP32-WROOM GPIO (as wired to the MSO5202D LA pod) --------
// Index i in this table IS the LA channel number (LA_PIN[3] = GPIO for L03).
static const uint8_t LA_PIN[16] = {
    13,  // L00
    12,  // L01   strapping (MTDI): must be LOW at boot — keep the LA probe hi-Z
    14,  // L02
    27,  // L03
    26,  // L04
    25,  // L05
    33,  // L06
    32,  // L07
    15,  // L08   strapping (MTDO)
     2,  // L09   strapping (also onboard LED on many boards)
     4,  // L10   strapping
    16,  // L11
    17,  // L12
     5,  // L13   strapping
    18,  // L14
    19,  // L15
};

// ---- pattern selection -------------------------------------------------------
#define PATTERN_FREQ    0
#define PATTERN_COUNTER 1
#ifndef PATTERN
#define PATTERN PATTERN_FREQ
#endif

// FREQ: half-period of channel N = HALF_US_BASE * (N+1).
// 500 us base -> L00 = 1 kHz (period 1 ms) ... L15 = 62.5 Hz (period 16 ms).
static const uint32_t HALF_US_BASE = 500;

// COUNTER: the counter increments every TICK_US; bit0 (L00) toggles each 2 ticks.
static const uint32_t TICK_US = 100;   // L00 = 5 kHz ... L15 ~ 0.076 Hz

static uint32_t nextToggle[16];
static uint8_t  pinState[16];
static uint32_t counter  = 0;
static uint32_t nextTick = 0;

#if PATTERN == PATTERN_COUNTER
// Write the 16-bit value across all pins *simultaneously* via the GPIO set/clear
// registers, so multi-bit transitions (e.g. 0111->1000) never glitch. Only our
// own pins are touched (W1TS/W1TC), never the rest of the port. GPIO32/33 live
// in the high register (out1), everything else in the low register (out).
static inline void writeCounter(uint32_t v) {
    uint32_t setLo = 0, clrLo = 0, setHi = 0, clrHi = 0;
    for (int i = 0; i < 16; i++) {
        const uint8_t p = LA_PIN[i];
        const bool on = (v >> i) & 1u;
        if (p < 32) { if (on) setLo |= (1u << p);        else clrLo |= (1u << p); }
        else        { if (on) setHi |= (1u << (p - 32)); else clrHi |= (1u << (p - 32)); }
    }
    GPIO.out_w1ts = setLo;      GPIO.out_w1tc = clrLo;
    GPIO.out1_w1ts.val = setHi; GPIO.out1_w1tc.val = clrHi;
}
#endif

void setup() {
    Serial.begin(115200);
    delay(200);
    const uint32_t now = micros();
    for (int i = 0; i < 16; i++) {
        pinMode(LA_PIN[i], OUTPUT);
        pinState[i] = 0;
        digitalWrite(LA_PIN[i], LOW);
        nextToggle[i] = now + HALF_US_BASE * (uint32_t)(i + 1);
    }
    nextTick = now + TICK_US;

#if PATTERN == PATTERN_FREQ
    Serial.println("\n[esp_toggler] MSO5202D LA test generator — FREQ mode");
    Serial.println("  LA  GPIO   freq");
    for (int i = 0; i < 16; i++)
        Serial.printf("  L%02d  D%-2u  %6.1f Hz\n",
                       i, LA_PIN[i], 1000.0 / (i + 1));
#else
    Serial.println("\n[esp_toggler] MSO5202D LA test generator — COUNTER mode");
    Serial.printf("  16-bit counter, tick = %u us (L00 = %.1f kHz)\n",
                  (unsigned)TICK_US, 500.0 / TICK_US);
#endif
    Serial.println("  3.3 V CMOS out — set LA threshold ~1.5 V.");
}

void loop() {
#if PATTERN == PATTERN_FREQ
    // 16 independent square waves; each pin flips when its own schedule is due.
    const uint32_t now = micros();
    for (int i = 0; i < 16; i++) {
        if ((int32_t)(now - nextToggle[i]) >= 0) {
            pinState[i] ^= 1u;
            digitalWrite(LA_PIN[i], pinState[i]);
            nextToggle[i] += HALF_US_BASE * (uint32_t)(i + 1);
        }
    }
#else
    const uint32_t now = micros();
    if ((int32_t)(now - nextTick) >= 0) {
        nextTick += TICK_US;
        writeCounter(counter++);
    }
#endif
}
