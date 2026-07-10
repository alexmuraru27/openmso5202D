// UART test-signal generator for the Hantek MSO5202D serial decoder.
//
// Drives an 8N1 UART line from an ESP-WROOM-32 using a HARDWARE UART peripheral
// (exact baud + framing — not a bit-bang), streaming the 0x00..0xFF ramp forever
// so the scope's UART decode can be verified byte-for-byte. Idle-high, 1 start
// bit, 8 data bits LSB-first, 1 stop bit. A short pause between bytes leaves an
// idle gap so frames are easy to see. Baud via -DBAUD (default 9600).
//
//   TX = GPIO13  -> scope CH1   (decoder --line 0)
//
// Only one line is used, so wire GPIO13 to CH1 (CH2 is unused for UART). Output
// is 3.3 V CMOS — share GND with the scope. Suggested timebase: 1 ms/div.

#include <Arduino.h>

static const uint8_t TX_PIN = 13;     // -> scope CH1

#ifndef BAUD
#define BAUD 9600
#endif

HardwareSerial uart(1);                  // UART1 peripheral, TX only
static uint8_t value = 0;

void setup() {
    Serial.begin(115200);
    delay(200);
    // UART1: BAUD 8N1, no RX pin (-1), TX on GPIO13. Hardware timing = exact.
    uart.begin(BAUD, SERIAL_8N1, -1, TX_PIN);
    Serial.println("\n[esp_uart_gen] MSO5202D UART test generator");
    Serial.printf("  TX = GPIO%u  -> scope CH1  (decoder --line 0)\n", TX_PIN);
    Serial.printf("  8N1, %lu baud (hardware UART), LSB-first, ramp 0x00..0xFF\n",
                  (unsigned long)BAUD);
    Serial.println("  3.3 V CMOS out — common GND with the scope. Timebase ~1 ms/div.");
    Serial.printf("  Decode: python3 mso5202d_decode.py decode cap.npz --proto uart --line 0 --baud %lu\n",
                  (unsigned long)BAUD);
}

void loop() {
    uart.write(value++);
    uart.flush();                        // ensure the byte is on the wire
    // ~3-bit idle gap: scales with baud (small at high baud, keeps density) and
    // lets the decoder re-sync each frame from idle instead of drifting.
    delayMicroseconds(3 * 1000000UL / BAUD);
}
