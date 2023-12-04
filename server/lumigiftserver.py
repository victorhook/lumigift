from ctypes import ArgumentError
from dataclasses import dataclass
from enum import IntEnum
from queue import Empty, Queue
import struct
from threading import Event, Thread
from time import sleep
import time
from typing import List
from crazyradio import Crazyradio


ADDRESS = (0x42, 0x42, 0x42, 0x42, 0x01)
CHANNEL = 80
DATARATE = Crazyradio.DR_250KPS
POWER = Crazyradio.P_0DBM
BROADCAST_FREQUENCY_HZ = 100

NBR_OF_LUMIGIFTS = 10


def as_hex_string(raw: bytes) -> str:
    return ' '.join(hex(a)[2:].zfill(2) for a in raw)

def get_radio() -> Crazyradio:
    try:
        radio = Crazyradio()
        radio.set_address(ADDRESS)
        radio.set_channel(CHANNEL)
        radio.set_data_rate(DATARATE)
        radio.set_power(POWER)
        radio.set_ack_enable(True)
        radio.set_cont_carrier(False)
    except Exception as e:
        print(e)
        radio = None

    return radio

class LumigiftPacketType(IntEnum):
    COLOR          = 0x01
    COLOR_WITH_ID  = 0x02
    SET_ID         = 0x03
    BLINK          = 0x04
    REBOOT         = 0x05

@dataclass
class LumigfitPacket:
    cmd: LumigiftPacketType
    raw: bytes

class PacketConstructor:

    def _hex_color_to_three_bytes(hex_color: str) -> bytes:
        return struct.pack(
            'BBB',
            int(hex_color[0:2], 16), # R
            int(hex_color[2:4], 16), # G
            int(hex_color[4:6], 16)  # B
        )

    @classmethod
    def make_color_individual_address_packet(cls, colors: List[str]) -> LumigfitPacket:
        pkt = struct.pack('B', LumigiftPacketType.COLOR_WITH_ID)
        for color in colors:
            pkt += cls._hex_color_to_three_bytes(color)

        return LumigfitPacket(cmd=LumigiftPacketType.COLOR_WITH_ID, raw=pkt)

    @classmethod
    def make_set_id_packet(cls, from_id: int, to_id: int) -> bytes:
        return LumigfitPacket(
            cmd=LumigiftPacketType.COLOR_WITH_ID,
            raw=struct.pack('BBB', LumigiftPacketType.SET_ID, from_id, to_id)
        )

    @classmethod
    def make_blink_packet(cls, id: int) -> bytes:
        return LumigfitPacket(
            cmd=LumigiftPacketType.BLINK,
            raw=struct.pack('BB', LumigiftPacketType.BLINK, id)
        )

    @classmethod
    def make_reboot_packet(cls, id: int) -> bytes:
        return LumigfitPacket(
            cmd=LumigiftPacketType.REBOOT,
            raw=struct.pack('BB', LumigiftPacketType.REBOOT, id)
        )


class LumigiftServer:

    packets_sent = 0

    def __init__(self, color_broadcast_frequency: int = BROADCAST_FREQUENCY_HZ) -> None:
        self._tx = Queue() # Queue[LumigfitPacket]
        self._stop_flag = Event()
        self._radio: Crazyradio = None
        self._colors: List[str] = ['000000' for i in range(NBR_OF_LUMIGIFTS)]
        self._color_broadcast_frequency = color_broadcast_frequency

        print('Initializing radio')
        self._radio = get_radio()
        print('Radio initialized')

        if self._radio is None:
            raise RuntimeError('Failed to connect to Crazy radio!')

        print(f'Using Crazyradio with address {ADDRESS}, channel {CHANNEL}, data rate: {DATARATE}, power: {POWER}')
        Thread(target=self._tx_thread, daemon=True).start()

    def start(self) -> None:
        self._stop_flag.clear()
        Thread(target=self._broadcast_thread, daemon=True).start()

    def stop(self) -> None:
        self._stop_flag.set()

    def set_colors(self, colors: List[str]) -> None:
        '''
        Sets the colors of the lumigifts with IDs: 0-NBR_OF_LUMIGIFTS.
        `colors` should be a list of NBR_OF_LUMIGIFTS hex color strings, for each lumigift
        '''
        if len(colors) != NBR_OF_LUMIGIFTS:
            raise ArgumentError(f'Expecting `colors` to be of size {NBR_OF_LUMIGIFTS}')
        self._colors = colors

    def blink_sequence(self, delay_ms: int = 1000) -> None:
        def _run():
            for i in range(NBR_OF_LUMIGIFTS):
                self.blink(i)
                time.sleep(delay_ms / 1000)

        Thread(target=_run, daemon=True).start()

    def set_id(self, from_id: int, to_id: int) -> None:
        self._tx.put(PacketConstructor.make_set_id_packet(from_id, to_id))

    def blink(self, id: int) -> None:
        self._tx.put(PacketConstructor.make_blink_packet(id))

    def reboot(self, id: int) -> None:
        self._tx.put(PacketConstructor.make_reboot_packet(id))

    def _broadcast_thread(self) -> None:
        print('Broadcasting started')

        period_s = 1.0 / self._color_broadcast_frequency

        while not self._stop_flag.is_set():
            next_broadcast = time.time() + period_s

            # Put broadcast packet in TX queue
            #print(self._colors)
            self._tx.put(PacketConstructor.make_color_individual_address_packet(self._colors))

            time_to_sleep = next_broadcast - time.time()
            if time_to_sleep > 0:
                sleep(time_to_sleep)

        print('Broadcasting stopped')

    def _tx_thread(self) -> None:
        while True:
            try:
                tx: LumigfitPacket = self._tx.get(block=True, timeout=1)
                #print(f'[{self.packets_sent}] TX: {tx.cmd.name}: {as_hex_string(tx.raw)} (len={len(tx.raw)})')
                self.packets_sent += 1
                res = self._radio.send_packet(tx.raw)
                #print(f'[TX {self.packets_sent}: {self._broadcast_color}] {len(tx)} bytes, res: {res.data}')
            except Empty:
                pass


if __name__ == '__main__':
    server = LumigiftServer()
    server.start()
