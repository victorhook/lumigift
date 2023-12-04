#include <Arduino.h>
#include <SPI.h>
#include "RF24.h"
#include <Adafruit_NeoPixel.h>
#include <EEPROM.h>


#define PIN_UART_RX PB7
#define PIN_UART_TX PC14
#define PIN_RGB_LED PA2
#define PIN_LED PB6
#define PIN_VBAT PA3
#define PIN_CSN PA11
#define PIN_CE PA8
#define PIN_MOSI PA7
#define PIN_MISO PA6
#define PIN_SCK PA5
#define PIN_IRQ PA4

#define NUM_LEDS 4

// -- Structs & Enums -- //
typedef struct
{
    uint8_t r;
    uint8_t g;
    uint8_t b;
}__attribute__((packed)) color_t;

typedef struct
{
    color_t led_1;
    color_t led_2;
    color_t led_3;
    color_t led_4;
}__attribute__((packed)) color_packet_t;

typedef struct
{
    color_t colors[10];  // We can have a total of 10 unique IDs
}__attribute__((packed)) color_with_id_packet_t;

typedef struct
{
    uint8_t from_id;
    uint8_t to_id;
}__attribute__((packed)) set_id_packet_t;

typedef struct
{
    uint8_t id;
}__attribute__((packed)) blink_packet_t;

typedef struct
{
    uint8_t id;
}__attribute__((packed)) reboot_packet_t;

typedef enum
{
    LUMIGIFT_PACKET_TYPE_COLOR          = 0x01,
    LUMIGIFT_PACKET_TYPE_COLOR_WITH_ID  = 0x02,
    LUMIGIFT_PACKET_TYPE_SET_ID         = 0x03,
    LUMIGIFT_PACKET_TYPE_BLINK          = 0x04,
    LUMIGIFT_PACKET_TYPE_REBOOT         = 0x05,
} lumigift_packet_type_t;

typedef union
{
    union
    {
        color_packet_t color;
        set_id_packet_t id;
    };
} lumigift_packet_payload_t;

typedef struct
{
    lumigift_packet_type_t packet_type;
    lumigift_packet_payload_t payload;
} lumigift_packet_t;

// -- Radio configs -- //
#define RADIO_POWER_LEVEL  RF24_PA_LOW
#define RADIO_CHANNEL      80
#define RADIO_ADDRESS_SIZE 5
#define RADIO_PAYLOAD_SIZE sizeof(lumigift_packet_t)
#define RADIO_DATA_RATE    RF24_250KBPS
#define RADIO_RX_PIPE      1

// -- Other defines -- //
#define PACKET_TIMEOUT_MS 500
#define EEPROM_ADDRESS_ID 0
#define DEFAULT_ID 0
#define MAX_ID     9

// -- Function definitions -- //
static void go_to_error();
static void blink_bootup_sequence();

/**
 * Sets the color to all LEDs
 */
static void set_color(const color_t color);

/**
 * Sets the color for each LED
 */
static void set_color_for_each_led(const color_t led_1, const color_t led_2, const color_t led_3, const color_t led_4);

static void blink(const uint32_t color);

static bool valid_id(const uint8_t id);


// -- Variables -- //
static RF24 radio(PIN_CE, PIN_CSN);
static uint8_t rx_address[RADIO_ADDRESS_SIZE]  = {0x42, 0x42, 0x42, 0x42, 0x01};
static uint8_t tx_address[RADIO_ADDRESS_SIZE] = {0x42, 0x42, 0x42, 0x42, 0x01};
static color_packet_t color = {0, 0, 0};
Adafruit_NeoPixel pixels(NUM_LEDS, PIN_RGB_LED, NEO_GRB + NEO_KHZ800);

static uint8_t current_id = 0;
static uint32_t last_packet_received = 0;
static bool connected = false;

void setup()
{
    // Initialize GPIO
    pinMode(PIN_LED, OUTPUT);
    pinMode(PIN_CSN, OUTPUT);
    pinMode(PIN_CE, OUTPUT);
    digitalWrite(PIN_LED, LOW);

    pixels.begin();
    pixels.clear();
    pixels.show();

    blink_bootup_sequence();

    // Uart for debug
    Serial.setTx(PIN_UART_TX);
    Serial.setRx(PIN_UART_RX);
    Serial.begin(115200);
    Serial.println("Initializing lumigift...");

    // SPI for radio
    SPI.setMOSI(PIN_MOSI);
    SPI.setMISO(PIN_MISO);
    SPI.setSCLK(PIN_SCK);

    // Radio
    if (!radio.begin(&SPI))
    {
        Serial.println("Failed to initialize radio!");
        go_to_error();
    }

    // Configure radio
    radio.setPALevel(RADIO_POWER_LEVEL);
    radio.enableDynamicPayloads();
    //radio.setPayloadSize(RADIO_PAYLOAD_SIZE);
    radio.setAddressWidth(RADIO_ADDRESS_SIZE);
    radio.setChannel(RADIO_CHANNEL);
    radio.setDataRate(RADIO_DATA_RATE);
    radio.setAutoAck(true);
    radio.openWritingPipe(tx_address);      // Uses pipe 0
    radio.openReadingPipe(RADIO_RX_PIPE, rx_address);
    radio.startListening();

    // Read our ID from EEPROM
    current_id = EEPROM.read(EEPROM_ADDRESS_ID);
    if (!valid_id(current_id))
    {
      Serial.printf("Found invalid ID in EEPROM (%d), using deafult, %d\n", current_id, DEFAULT_ID);
      set_id(DEFAULT_ID);
    }

    Serial.printf("Boot complete, starting to run with ID %d\n", current_id);
}

void loop()
{
	//Serial.println("Trying to read ")
    uint8_t rx_pipe;
    uint8_t rx_buf[RADIO_PAYLOAD_SIZE];
    lumigift_packet_payload_t* rx;
    color_packet_t* color;

    color_with_id_packet_t* color_with_id_packet;
    set_id_packet_t* set_id_packet;
    blink_packet_t* blink_packet;
    reboot_packet_t* reboot_packet;

    if (radio.available(&rx_pipe))
    {
        connected = true;
        uint8_t size = radio.getPayloadSize();
        radio.read(rx_buf, size);

        lumigift_packet_type_t packet_type = (lumigift_packet_type_t) rx_buf[0];
        uint8_t* packet_payload = &rx_buf[1];
        last_packet_received = millis();
        Serial.printf("[My ID: %d] New packet: %d, %d bytes\n", current_id, packet_type, size);

        switch (packet_type)
        {
            case LUMIGIFT_PACKET_TYPE_COLOR:
                color = (color_packet_t*) packet_payload;
                set_color_for_each_led(color->led_1, color->led_2, color->led_3, color->led_4);
                for (int i = 0; i < size; i++)
                {
                    Serial.printf("%02x ", ((uint8_t*) &rx_buf)[i]);
                }

                //Serial.printf("Color packet received: #%02x%02x%02x\n", rx.payload.color.r, rx.payload.color.g, rx.payload.color.b);
                //Serial.printf("1: #%02x%02x%02x, 2: #%02x%02x%02x, 3, #%02x%02x%02x, 4: #%02x%02x%02x", )
                break;
            case LUMIGIFT_PACKET_TYPE_COLOR_WITH_ID:
                color_with_id_packet = (color_with_id_packet_t*) packet_payload;
                Serial.printf(
                  "%02x%02x%02x\n", 
                  color_with_id_packet->colors[current_id].r,
                  color_with_id_packet->colors[current_id].g,
                  color_with_id_packet->colors[current_id].b
                );
                set_color(color_with_id_packet->colors[current_id]);
                break;
            case LUMIGIFT_PACKET_TYPE_SET_ID:
                set_id_packet = (set_id_packet_t*) packet_payload;
                //Serial.printf("CONFIG: %d -> \n", set_id_packet->from_id, set_id_packet->to_id);
                if (set_id_packet->from_id == current_id)
                {
                  if (valid_id(set_id_packet->to_id))
                  {
                    Serial.printf("Switching ID from %d to %d!\n", set_id_packet->from_id, set_id_packet->to_id);
                    set_id(set_id_packet->to_id);
                    blink(0x00FF00);
                  }
                }
                break;
            case LUMIGIFT_PACKET_TYPE_BLINK:
                blink_packet = (blink_packet_t*) packet_payload;
                //Serial.printf("BLINK: %d\n", blink_packet->id);
                if (blink_packet->id == current_id)
                {
                  blink(0x00FFFF);
                }
                break;
            case LUMIGIFT_PACKET_TYPE_REBOOT:
                reboot_packet = (reboot_packet_t*) packet_payload;
                if (reboot_packet->id == current_id)
                {
                  // Let's reboot the system
                  blink(0xFF0000);
                  NVIC_SystemReset();
                }
                //Serial.printf("REBOOT: %d\n", reboot_packet->id);
                break;
        }

    }

    // Check if it's been too long since packet received, if so, we'll turn off LEDs to save power
    if (connected && (millis() - last_packet_received) > PACKET_TIMEOUT_MS)
    {
        Serial.println("Disconnected!");
        pixels.clear();
        pixels.show();
        connected = false;
    }
}

static void set_color(const color_t color)
{
    pixels.clear();
    pixels.setPixelColor(0, color.r, color.g, color.b);
    pixels.setPixelColor(1, color.r, color.g, color.b);
    pixels.setPixelColor(2, color.r, color.g, color.b);
    pixels.setPixelColor(3, color.r, color.g, color.b);   
    pixels.show();
}

static void set_color_for_each_led(const color_t led_1, const color_t led_2, const color_t led_3, const color_t led_4)
{
    pixels.clear();
    pixels.setPixelColor(0, led_1.r, led_1.g, led_1.b);
    pixels.setPixelColor(1, led_2.r, led_2.g, led_2.b);
    pixels.setPixelColor(2, led_3.r, led_3.g, led_3.b);
    pixels.setPixelColor(3, led_4.r, led_4.g, led_4.b);   
    pixels.show();
}


static void go_to_error()
{
    while (1)
    {
        digitalWrite(PIN_LED, HIGH);
        delay(1000);
        digitalWrite(PIN_LED, LOW);
        delay(1000);
    }
}


static void blink_bootup_sequence()
{
  blink(0x0000FF);
}

static void blink(const uint32_t color)
{
  for (int i = 0; i < 3; i++)
    {
        pixels.fill(color, 0, NUM_LEDS);
        pixels.show();
        delay(100);
        pixels.clear();
        pixels.show();
        delay(100);
    }
}

static bool valid_id(const uint8_t current_id)
{
  return (current_id >= 0) && (current_id <= MAX_ID);
}

static void set_id(const uint8_t new_id)
{
  EEPROM.write(EEPROM_ADDRESS_ID, new_id);
  current_id = new_id;
}
