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
// Serial protocol AND frequency are now switchable AT RUNTIME over the USB serial
// console (115200 baud) — no reflashing to change protocol or bit rate. Each
// protocol has a discrete TABLE of frequencies (see *_TAB below); `freq <hz>`
// snaps to the nearest entry:
//   SPI : SCLK=CH1(22)  MOSI=CH2(23)   mode 0, MSB, 1 kHz .. 20 MHz (HW peripheral)
//   UART: TX=CH1(22)    (CH2 unused)   8N1,       300 bps .. 5 Mbps  (HW peripheral)
//   I2C : SCL=CH1(22)   SDA=CH2(23)    self-ACK,  1 kHz .. 5 MHz nominal (bit-banged;
//                                      actually reaches ~500 kHz — see freq_achieved)
// All loop the 0x00..0xFF ramp. 3.3 V CMOS out — common GND with the scope.
//
// Serial command API (send one command per line, 115200 8N1). Every command
// replies with a single JSON line; unknown/malformed lines reply {"ok":false,...}.
//   help | ?          -> usage (JSON, "help" field)
//   id | ping         -> {"ok":true,"dev":"esp_combo_gen",...}
//   status            -> full state (proto, freq, achieved, per-proto ranges)
//   range             -> current protocol's [min,max]
//   proto spi|uart|i2c-> switch protocol (restores that protocol's last freq)
//   freq <hz>         -> set frequency for the current protocol (clamped to range)
//   set <proto> <hz>  -> switch protocol AND set its frequency in one call
// See README.md and scripts/mso5202d_espgen.py (host-side control tool).

#include <Arduino.h>
#include <SPI.h>

// ---------------- LA: 16 channels, distinct frequency each ----------------
// Index i = LA channel Di. f_N = 1000/(N+1) Hz (D0 = 1 kHz … D15 = 62.5 Hz).
static const uint8_t LA_PIN[16] = {
    13, 12, 14, 27, 26, 25, 33, 32, 15, 2, 4, 16, 17, 5, 18, 19,
};
static const uint32_t LA_HALF_US_BASE = 500; // half-period of Dn = 500us*(n+1)
static uint32_t laNext[16];
static uint8_t laState[16];

static inline void laInit(uint32_t now)
{
    for (int i = 0; i < 16; i++)
    {
        pinMode(LA_PIN[i], OUTPUT);
        laState[i] = 0;
        digitalWrite(LA_PIN[i], LOW);
        laNext[i] = now + LA_HALF_US_BASE * (uint32_t)(i + 1);
    }
}
// Re-assert the LA pin modes/levels without disturbing the running schedule.
// Called after any (re)start of the CH1/CH2 peripheral: SPI.begin()/end() can
// leave GPIO18/19 (= LA D14/D15) grabbed as VSPI SCK/MISO even when we pass
// explicit pins, so making pinMode(OUTPUT) the last thing to touch every LA pin
// lets the LA reclaim them.
static inline void laReclaimPins()
{
    for (int i = 0; i < 16; i++)
    {
        pinMode(LA_PIN[i], OUTPUT);
        digitalWrite(LA_PIN[i], laState[i]);
    }
}
// Non-blocking: flip any Dn whose half-period elapsed. Cheap (16 compares).
static inline void laTick(uint32_t now)
{
    for (int i = 0; i < 16; i++)
    {
        if ((int32_t)(now - laNext[i]) >= 0)
        {
            laState[i] ^= 1u;
            digitalWrite(LA_PIN[i], laState[i]);
            laNext[i] += LA_HALF_US_BASE * (uint32_t)(i + 1);
        }
    }
}

// ---------------- serial on CH1/CH2 ----------------
#define P_SPI 0
#define P_UART 1
#define P_I2C 2
#ifndef PROTO // build-time DEFAULT protocol (runtime-switchable)
#define PROTO P_SPI
#endif

static const uint8_t CH1 = 22; // scope CH1
static const uint8_t CH2 = 23; // scope CH2

// Per-protocol frequency TABLE (Hz) — a discrete ladder of useful/standard rates.
// `freq <hz>` snaps to the nearest entry. table[0]/table[N-1] are the min/max.
//   SPI  : SCLK, 1 kHz .. 20 MHz (HW peripheral rounds to its nearest divisor)
//   UART : baud, standard bauds 300 .. 5 Mbaud (ESP32 HW UART ceiling)
//   I2C  : SCL, standard I2C modes; bit-bang can only *reach* ~500 kHz, so the
//          1M/3.4M/5M entries are requestable but report freq_achieved ~500 kHz.
static const uint32_t SPI_TAB[] = {
    1000, 10000, 50000, 100000, 250000, 500000, 1000000, 2000000, 4000000, 5000000, 8000000, 10000000, 12000000, 16000000, 20000000,
};
static const uint32_t UART_TAB[] = {
    300, 1200, 2400, 4800, 9600, 14400, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000, 1500000, 2000000, 3000000, 5000000,
};
static const uint32_t I2C_TAB[] = {
    1000, 10000, 50000, 100000, 400000, 1000000, 3400000, 5000000,
};
static const uint32_t *const FREQ_TAB[3] = {SPI_TAB, UART_TAB, I2C_TAB};
static const uint8_t FREQ_N[3] = {
    (uint8_t)(sizeof(SPI_TAB) / sizeof(SPI_TAB[0])),
    (uint8_t)(sizeof(UART_TAB) / sizeof(UART_TAB[0])),
    (uint8_t)(sizeof(I2C_TAB) / sizeof(I2C_TAB[0])),
};
static const char *PROTO_NAME[3] = {"spi", "uart", "i2c"};
static uint32_t freqFor[3] = {1000000, 115200, 100000}; // defaults per protocol

static uint8_t curProto = PROTO;
static uint32_t curFreq = 1000000;
static uint8_t value = 0; // ramp byte
static uint32_t nextSerial = 0;

// I2C bit-bang timing (derived from curFreq when the protocol is I2C).
static uint32_t i2cQuarterUs = 5;

// ---- burst / gap: continuous-vs-framed transmit control ----
// Each "unit" sends `effBurst()` ramp bytes in ONE transaction (SPI: one
// begin/endTransaction; UART: one write; I2C: one START..STOP), then idles for
// `effGap()` microseconds before the next. Large burst + gap 0 = a near-gapless
// continuous stream on the scope; burst 1 + a gap = framed bytes the serial
// decoders can reframe. `mode single|continuous` presets both; `burst`/`gap`
// override individually.
static uint32_t computeGapUs(uint32_t freq); // fwd decl
static const uint16_t BURST_MAX = 256;
static uint8_t txbuf[BURST_MAX];
static const uint16_t protoBurst[3] = {1, 1, 4}; // framed default per protocol
static int32_t userBurst = -1;                   // <0 => use protoBurst default
static int32_t userGap = -1;                     // <0 => auto gap (computeGapUs)

static uint16_t effBurst()
{
    int32_t b = (userBurst > 0) ? userBurst : protoBurst[curProto];
    if (b < 1)
        b = 1;
    if (b > BURST_MAX)
        b = BURST_MAX;
    return (uint16_t)b;
}
static uint32_t effGap() { return (userGap >= 0) ? (uint32_t)userGap : computeGapUs(curFreq); }

SPIClass spi(VSPI);
HardwareSerial uart(1);

// ---- per-protocol peripheral start/stop + one transmit "unit" ----
static void spiStart() { spi.begin(CH1, -1, CH2, -1); } // SCK=22, MOSI=23
static void spiStop() { spi.end(); }
static void spiUnit()
{
    uint16_t n = effBurst();
    for (uint16_t i = 0; i < n; i++)
        txbuf[i] = value++;
    spi.beginTransaction(SPISettings(curFreq, MSBFIRST, SPI_MODE0));
    spi.writeBytes(txbuf, n); // n bytes, continuous SCLK, no inter-byte gap
    spi.endTransaction();
}

static void uartStart() { uart.begin(curFreq, SERIAL_8N1, -1, CH1); } // TX=22
static void uartStop() { uart.end(); }
static void uartUnit()
{
    uint16_t n = effBurst();
    for (uint16_t i = 0; i < n; i++)
        txbuf[i] = value++;
    uart.write(txbuf, n); // back-to-back frames (no idle between bytes)
    if (effGap() > 0)
        uart.flush(); // only wait for drain when a gap follows
}

// Bit-banged self-ACK I2C master (no slave needed — SDA is driven for the ACK
// bit too, so the ramp always "acks"). SCL period ~= 2 * i2cQuarterUs.
static inline void sclSet(int v)
{
    digitalWrite(CH1, v);
    delayMicroseconds(i2cQuarterUs);
}
static inline void sdaSet(int v)
{
    digitalWrite(CH2, v);
    delayMicroseconds(i2cQuarterUs);
}
static inline void i2cBit(int b)
{
    sdaSet(b);
    sclSet(1);
    sclSet(0);
}
static inline void i2cByte(uint8_t v)
{
    for (int i = 7; i >= 0; i--)
        i2cBit((v >> i) & 1u);
    i2cBit(0);
}
static void i2cStart()
{
    pinMode(CH1, OUTPUT);
    pinMode(CH2, OUTPUT);
    digitalWrite(CH1, HIGH);
    digitalWrite(CH2, HIGH);
}
static void i2cStop() {}
static void i2cUnit()
{
    static const uint8_t ADDR = 0x50;
    uint16_t n = effBurst();
    sdaSet(1);
    sclSet(1);
    sdaSet(0);
    sclSet(0); // START
    i2cByte((ADDR << 1) | 0);
    for (uint16_t k = 0; k < n; k++)
        i2cByte(value++); // n ramp bytes / transaction
    sdaSet(0);
    sclSet(1);
    sdaSet(1); // STOP
}

// Idle gap between transmit units: ~30 bit-times (so the decoder can reframe),
// floored at 200us. bit_us = 1e6/freq.
static uint32_t computeGapUs(uint32_t freq)
{
    uint32_t g = (uint32_t)(30.0 * 1e6 / (double)freq);
    return g < 200 ? 200 : g;
}

static void applyFreqInternal()
{
    // Reconfigure whatever the current protocol needs for curFreq.
    if (curProto == P_UART)
    {
        uart.end();
        uart.begin(curFreq, SERIAL_8N1, -1, CH1);
        laReclaimPins();
    }
    else if (curProto == P_I2C)
    {
        uint32_t q = (uint32_t)(500000.0 / (double)curFreq + 0.5); // half SCL / 2
        i2cQuarterUs = q < 1 ? 1 : q;
    }
    // SPI reads curFreq per transaction — nothing to reconfigure.
    // (gap is computed on the fly via effGap(), so nothing to cache here.)
}

// Frequency the hardware actually applies for curFreq (SPI/UART == requested to
// first order; I2C is quantised to the integer-microsecond bit-bang delay).
static uint32_t achievedFreq()
{
    if (curProto == P_I2C)
        return (uint32_t)(500000.0 / (double)i2cQuarterUs + 0.5);
    return curFreq;
}

static void serialUnit()
{
    if (curProto == P_SPI)
        spiUnit();
    else if (curProto == P_UART)
        uartUnit();
    else
        i2cUnit();
}

static void stopCurrent()
{
    if (curProto == P_SPI)
        spiStop();
    else if (curProto == P_UART)
        uartStop();
    else
        i2cStop();
}
static void startCurrent()
{
    if (curProto == P_SPI)
        spiStart();
    else if (curProto == P_UART)
        uartStart();
    else
        i2cStart();
    laReclaimPins();
}

// Switch to protocol p (0/1/2), restoring that protocol's remembered frequency.
static void setProto(uint8_t p)
{
    stopCurrent();
    curProto = p;
    curFreq = freqFor[p];
    startCurrent();
    applyFreqInternal();
    value = 0;
    nextSerial = micros() + effGap();
}

// Nearest frequency in protocol p's table to f.
static uint32_t snapFreq(uint8_t p, uint32_t f)
{
    const uint32_t *t = FREQ_TAB[p];
    uint8_t n = FREQ_N[p];
    uint32_t best = t[0];
    uint32_t bd = (f > t[0]) ? (f - t[0]) : (t[0] - f);
    for (uint8_t i = 1; i < n; i++)
    {
        uint32_t d = (f > t[i]) ? (f - t[i]) : (t[i] - f);
        if (d < bd)
        {
            bd = d;
            best = t[i];
        }
    }
    return best;
}

// Set frequency for the current protocol, snapped to the nearest table entry.
static uint32_t setFreq(uint32_t f)
{
    curFreq = snapFreq(curProto, f);
    freqFor[curProto] = curFreq;
    applyFreqInternal();
    return curFreq;
}

// ---------------- serial command API ----------------
static char cmdBuf[96];
static size_t cmdLen = 0;

// Print a table as a JSON array "[a,b,c]".
static void printArr(const uint32_t *t, uint8_t n)
{
    Serial.print('[');
    for (uint8_t i = 0; i < n; i++)
    {
        if (i)
            Serial.print(',');
        Serial.print((unsigned long)t[i]);
    }
    Serial.print(']');
}

static void printStatus()
{
    uint8_t p = curProto;
    Serial.printf("{\"ok\":true,\"proto\":\"%s\",\"freq\":%lu,\"freq_achieved\":%lu,"
                  "\"min\":%lu,\"max\":%lu,\"value\":%u,\"protos\":[\"spi\",\"uart\",\"i2c\"],",
                  PROTO_NAME[p], (unsigned long)curFreq, (unsigned long)achievedFreq(), (unsigned long)FREQ_TAB[p][0],
                  (unsigned long)FREQ_TAB[p][FREQ_N[p] - 1], value);
    // `tables` carries every protocol's ladder; the active one is tables[proto],
    // so no separate redundant "table" field.
    Serial.print("\"tables\":{\"spi\":");
    printArr(SPI_TAB, FREQ_N[0]);
    Serial.print(",\"uart\":");
    printArr(UART_TAB, FREQ_N[1]);
    Serial.print(",\"i2c\":");
    printArr(I2C_TAB, FREQ_N[2]);
    Serial.printf("},\"freqs\":{\"spi\":%lu,\"uart\":%lu,\"i2c\":%lu},", (unsigned long)freqFor[0], (unsigned long)freqFor[1],
                  (unsigned long)freqFor[2]);
    uint32_t g = effGap();
    Serial.printf("\"burst\":%u,\"gap_us\":%lu,\"continuous\":%s,\"mode\":\"%s\",", effBurst(), (unsigned long)g, g == 0 ? "true" : "false",
                  g == 0 ? "continuous" : "framed");
    Serial.print("\"la\":{\"channels\":16,\"fmt\":\"f_N=1000/(N+1)Hz\"}}\n");
}

static void replyErr(const char *msg) { Serial.printf("{\"ok\":false,\"error\":\"%s\"}\n", msg); }

static int parseProto(const char *s)
{
    for (int i = 0; i < 3; i++)
        if (strcasecmp(s, PROTO_NAME[i]) == 0)
            return i;
    if (strcasecmp(s, "usart") == 0)
        return P_UART; // accept the alias
    return -1;
}

static void handleCommand(char *line)
{
    // tokenize on spaces
    char *tok = strtok(line, " \t");
    if (!tok)
        return; // blank line: ignore silently

    if (!strcasecmp(tok, "help") || !strcmp(tok, "?"))
    {
        Serial.print("{\"ok\":true,\"help\":\"cmds: id | status | range/list | "
                     "proto <spi|uart|i2c> | freq <hz> | set <proto> <hz> | "
                     "burst <1..256> | gap <us|auto> | mode <single|continuous>. "
                     "freq is SPI SCLK / UART baud / I2C SCL, snapped to the nearest "
                     "table entry (see 'list'). burst=bytes per transaction, gap=idle "
                     "us between them (0=continuous). replies are JSON.\"}\n");
        return;
    }
    if (!strcasecmp(tok, "id") || !strcasecmp(tok, "ping"))
    {
        Serial.print("{\"ok\":true,\"dev\":\"esp_combo_gen\",\"api\":1}\n");
        return;
    }
    if (!strcasecmp(tok, "status") || !strcasecmp(tok, "stat"))
    {
        printStatus();
        return;
    }
    if (!strcasecmp(tok, "range") || !strcasecmp(tok, "list"))
    {
        uint8_t p = curProto;
        Serial.printf("{\"ok\":true,\"proto\":\"%s\",\"min\":%lu,\"max\":%lu,\"table\":", PROTO_NAME[p], (unsigned long)FREQ_TAB[p][0],
                      (unsigned long)FREQ_TAB[p][FREQ_N[p] - 1]);
        printArr(FREQ_TAB[p], FREQ_N[p]);
        Serial.print("}\n");
        return;
    }
    if (!strcasecmp(tok, "proto"))
    {
        char *a = strtok(NULL, " \t");
        int p = a ? parseProto(a) : -1;
        if (p < 0)
        {
            replyErr("proto must be spi|uart|i2c");
            return;
        }
        setProto((uint8_t)p);
        printStatus();
        return;
    }
    if (!strcasecmp(tok, "freq"))
    {
        char *a = strtok(NULL, " \t");
        if (!a)
        {
            replyErr("freq needs a value in Hz");
            return;
        }
        uint32_t f = (uint32_t)strtoul(a, NULL, 10);
        if (f == 0)
        {
            replyErr("freq must be a positive integer");
            return;
        }
        setFreq(f);
        printStatus();
        return;
    }
    if (!strcasecmp(tok, "set"))
    {
        char *a = strtok(NULL, " \t");
        char *b = strtok(NULL, " \t");
        int p = a ? parseProto(a) : -1;
        if (p < 0)
        {
            replyErr("set <proto> <hz>: bad proto");
            return;
        }
        setProto((uint8_t)p);
        if (b)
        {
            uint32_t f = (uint32_t)strtoul(b, NULL, 10);
            if (f > 0)
                setFreq(f);
        }
        printStatus();
        return;
    }
    if (!strcasecmp(tok, "burst"))
    {
        char *a = strtok(NULL, " \t");
        if (!a)
        {
            replyErr("burst needs a byte count 1..256");
            return;
        }
        long n = strtol(a, NULL, 10);
        if (n < 1 || n > BURST_MAX)
        {
            replyErr("burst out of range 1..256");
            return;
        }
        userBurst = (int32_t)n;
        printStatus();
        return;
    }
    if (!strcasecmp(tok, "gap"))
    {
        char *a = strtok(NULL, " \t");
        if (!a || (strcasecmp(a, "auto") == 0))
        {
            userGap = -1;
            printStatus();
            return;
        }
        long g = strtol(a, NULL, 10);
        if (g < 0)
        {
            replyErr("gap must be >= 0 us (or 'auto')");
            return;
        }
        userGap = (int32_t)g;
        printStatus();
        return;
    }
    if (!strcasecmp(tok, "mode"))
    {
        char *a = strtok(NULL, " \t");
        if (!a)
        {
            replyErr("mode <single|continuous>");
            return;
        }
        if (!strcasecmp(a, "single") || !strcasecmp(a, "framed"))
        {
            userBurst = -1;
            userGap = -1; // per-proto default + auto gap
        }
        else if (!strcasecmp(a, "cont") || !strcasecmp(a, "continuous") || !strcasecmp(a, "stream"))
        {
            userBurst = 64;
            userGap = 0; // long bursts, no idle gap
        }
        else
        {
            replyErr("mode must be single|continuous");
            return;
        }
        printStatus();
        return;
    }
    replyErr("unknown command (try 'help')");
}

static void pollCommands()
{
    while (Serial.available())
    {
        char c = (char)Serial.read();
        if (c == '\n' || c == '\r')
        {
            if (cmdLen)
            {
                cmdBuf[cmdLen] = '\0';
                handleCommand(cmdBuf);
                cmdLen = 0;
            }
        }
        else if (cmdLen < sizeof(cmdBuf) - 1)
        {
            cmdBuf[cmdLen++] = c;
        }
        else
        {
            cmdLen = 0; // overrun: drop the line
        }
    }
}

void setup()
{
    Serial.begin(115200);
    delay(200);
    // Bring the serial peripheral up FIRST (see laReclaimPins() note), then LA.
    curProto = PROTO;
    curFreq = freqFor[curProto];
    startCurrent();
    applyFreqInternal();
    const uint32_t now = micros();
    laInit(now);
    laReclaimPins();
    nextSerial = now + effGap();

    Serial.println("\n[esp_combo_gen] MSO5202D combined analog + LA generator");
    Serial.println("  runtime protocol + frequency control over this serial link (JSON).");
    Serial.println("  cmds: help | id | status | list | proto | freq <hz> | set <proto> <hz> | burst <n> | gap <us> | mode <single|continuous>");
    Serial.println("  freq snaps to a per-protocol table (SPI 1kHz..20MHz, UART 300..5Mbaud, I2C 1kHz..5MHz)");
    Serial.println("  'mode continuous' = long bursts, no gap (solid stream); 'mode single' = framed bytes (decoder-friendly)");
    Serial.println("  CH1=GPIO22  CH2=GPIO23  LA D0..D15 on 13,12,14,27,26,25,33,32,15,2,4,16,17,5,18,19");
    Serial.println("  LA f_N=1000/(N+1)Hz (D0=1kHz..D15=62.5Hz). 3.3V CMOS, common GND, LA thresh ~1.5V.");
    printStatus();
}

void loop()
{
    pollCommands(); // handle host control commands
    const uint32_t now = micros();
    laTick(now); // keep all 16 LA lines toggling
    if ((int32_t)(now - nextSerial) >= 0)
    { // pace the serial, non-blocking
        serialUnit();
        nextSerial = micros() + effGap();
    }
}
