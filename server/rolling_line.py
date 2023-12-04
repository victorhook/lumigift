from typing import List
from lumigiftserver import LumigiftServer
import time


nbr_leds = 10


def c(color_as_u8: int) -> str:
    return hex(color_as_u8)[2:].zfill(2)


def blank() -> List[str]:
    return ['000000' for i in range(nbr_leds)]


if __name__ == '__main__':
    server = LumigiftServer()

    length = 7
    server.set_colors(blank())
    server.start()

    delay = .05
    leader = 0
    strength_inc = .15
    while True:

        for i in range(0, nbr_leds):
            colors = blank()

            # We'll interpolate between start and end?
            strength = 1.0
            for j in reversed(range(max(i - length, 0), i+1)):
                colors[j] = f'{c(int(255 * strength))}0000'
                strength -= strength_inc
                if strength < 0:
                    strength = 0

            #colors[i] = 'ff0000'
            print(colors)
            server.set_colors(colors)
            time.sleep(delay)

        for i in reversed(range(0, nbr_leds-1)):
            colors = blank()
            strength = 1.0
            for j in range(i, min(i + length, nbr_leds)):
                colors[j] = f'{c(int(255 * strength))}0000'
                strength -= strength_inc
                if strength < 0:
                    strength = 0

            print(colors)
            server.set_colors(colors)
            time.sleep(delay)

