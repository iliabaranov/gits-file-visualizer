#!/usr/bin/env python3
"""Generates real filesystem activity for the live wallpaper to show.

    tools/stress.py                     # the full show, about 3 minutes
    tools/stress.py --rate 3            # three times as busy
    tools/stress.py --phase burst --seconds 30
    tools/stress.py --keep              # leave the files (default: delete at the end)

Everything happens in one folder (default ~/gits-stress), which the collector
watches because it is under your home and not excluded. Phases:

  setup    create folders and files, then wait for the tower to show them
           (new folders appear at the next layout rebuild, at most ~20 s)
  edit     a few files rewritten slowly           -> crimson flashes, spin boosts
  build    many reads + writes + new object files  -> the folder unwinds, cream/crimson storm
  browse   files read one after another            -> cream flashes, labels
  cleanup  a batch of files deleted                -> segments fall into gold flakes
  burst    everything at once, as fast as --rate allows
  teardown the whole folder deleted (unless --keep) -> a big fall of flakes

Watch with the desktop visible (press Super for the overview, or minimize
windows); the wallpaper pauses while windows cover it.
"""

import argparse
import os
import random
import shutil
import sys
import time

WORDS = ("motor driver config sensor bridge state queue buffer parser render stop safety relay frame "
         "packet node filter control power monitor camera lidar imu gpio uart spi display panel widget "
         "utils common types error main server client proto message timer clock task worker pool cache "
         "store index layout shader mesh tower ring segment event watch scan tree path trace").split()


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


class Stress:
    def __init__(self, root, rate, seed):
        self.root = root
        self.rate = rate
        self.rng = random.Random(seed)
        self.files = []  # paths that exist
        self.ops = 0

    # --- primitives -----------------------------------------------------------

    def write(self, path, size=None):
        size = size if size is not None else int(self.rng.lognormvariate(8.5, 1.3))
        with open(path, "wb") as f:
            f.write(os.urandom(min(size, 4 << 20)))
        self.ops += 1

    def append(self, path):
        with open(path, "ab") as f:
            f.write(os.urandom(self.rng.randint(16, 2048)))
        self.ops += 1

    def read(self, path):
        with open(path, "rb") as f:
            f.read(65536)
        self.ops += 1

    def pick(self, sub=None):
        pool = [p for p in self.files if sub is None or f"/{sub}/" in p]
        return self.rng.choice(pool) if pool else None

    def sleep_rate(self, per_second):
        time.sleep(1.0 / max(per_second * self.rate, 0.1))

    def run_for(self, seconds, per_second, step):
        end = time.time() + seconds
        while time.time() < end:
            step()
            self.sleep_rate(per_second)

    # --- phases ---------------------------------------------------------------

    def setup(self, wait):
        log(f"setup: creating folders in {self.root}")
        layout = {
            "project/src": 40, "project/src/core": 30, "project/src/net": 25, "project/tests": 20,
            "project/build/obj": 120, "project/build/gen": 40, "project/docs": 12,
            "photos/2026-09": 60, "photos/2026-10": 45, "logs": 15,
        }
        for sub, n in layout.items():
            d = os.path.join(self.root, sub)
            os.makedirs(d, exist_ok=True)
            ext = {"photos": ".jpg", "logs": ".log", "build": ".o", "docs": ".md"}.get(sub.split("/")[0], ".rs")
            if "build" in sub:
                ext = ".o"
            for i in range(n):
                name = f"{self.rng.choice(WORDS)}_{i:03d}{ext}"
                p = os.path.join(d, name)
                size = int(self.rng.lognormvariate(14.5, 0.4)) if ext == ".jpg" else None
                self.write(p, size)
                self.files.append(p)
        log(f"setup: {len(self.files)} files in {len(layout)} folders; waiting {wait:.0f} s for the tower to show them")
        time.sleep(wait)

    def edit(self, seconds):
        log("edit: a few source files rewritten slowly (crimson flashes)")
        focus = [self.pick("src") for _ in range(3)]
        self.run_for(seconds, 1.5, lambda: self.append(self.rng.choice(focus)))

    def build(self, seconds):
        log("build: reading sources, writing and creating object files (the folder unwinds)")
        objdir = os.path.join(self.root, "project/build/obj")

        def step():
            r = self.rng.random()
            if r < 0.5:
                p = self.pick("src")
                p and self.read(p)
            elif r < 0.9:
                p = self.pick("obj")
                p and self.write(p)
            else:
                p = os.path.join(objdir, f"{self.rng.choice(WORDS)}-{self.rng.getrandbits(40):010x}.o")
                self.write(p)
                self.files.append(p)

        self.run_for(seconds, 40, step)

    def browse(self, seconds):
        log("browse: photos opened one after another (cream flashes, labels)")
        photos = sorted(p for p in self.files if "/photos/" in p)
        it = iter(photos * 10)
        self.run_for(seconds, 2.5, lambda: self.read(next(it)))

    def cleanup(self, fraction=0.5):
        victims = [p for p in self.files if "/build/obj/" in p]
        self.rng.shuffle(victims)
        victims = victims[: int(len(victims) * fraction)]
        log(f"cleanup: deleting {len(victims)} object files (segments fall into gold flakes)")
        for p in victims:
            try:
                os.remove(p)
                self.files.remove(p)
                self.ops += 1
            except FileNotFoundError:
                pass
            self.sleep_rate(60)

    def burst(self, seconds):
        log("burst: everything at once")
        gen = os.path.join(self.root, "project/build/gen")

        def step():
            r = self.rng.random()
            if r < 0.35:
                p = self.pick()
                p and self.read(p)
            elif r < 0.7:
                p = self.pick()
                p and (self.append(p) if p.endswith((".rs", ".log", ".md")) else self.write(p))
            elif r < 0.85:
                p = os.path.join(gen, f"gen_{self.rng.getrandbits(32):08x}.rs")
                self.write(p)
                self.files.append(p)
            else:
                p = self.pick("gen") or self.pick("obj")
                if p:
                    try:
                        os.remove(p)
                        self.files.remove(p)
                        self.ops += 1
                    except FileNotFoundError:
                        pass

        self.run_for(seconds, 120, step)

    def teardown(self):
        log(f"teardown: deleting {self.root}")
        shutil.rmtree(self.root, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dir", default=os.path.expanduser("~/gits-stress"), help="working folder (default ~/gits-stress)")
    ap.add_argument("--rate", type=float, default=1.0, help="activity multiplier (default 1)")
    ap.add_argument("--phase", choices=["all", "edit", "build", "browse", "cleanup", "burst"], default="all")
    ap.add_argument("--seconds", type=float, default=25, help="length of each timed phase (default 25)")
    ap.add_argument("--wait", type=float, default=25, help="pause after creating files (default 25)")
    ap.add_argument("--keep", action="store_true", help="do not delete the folder at the end")
    ap.add_argument("--seed", type=int, default=2501)
    a = ap.parse_args()

    root = os.path.abspath(a.dir)
    home = os.path.expanduser("~")
    if not root.startswith(home + os.sep) or root == home:
        sys.exit(f"refusing to use {root}: it must be a folder inside {home}")
    if os.path.exists(root) and os.listdir(root) and not os.path.exists(os.path.join(root, ".gits-stress")):
        sys.exit(f"refusing to use {root}: it exists and was not made by this script")
    os.makedirs(root, exist_ok=True)
    open(os.path.join(root, ".gits-stress"), "w").close()

    s = Stress(root, a.rate, a.seed)
    t0 = time.time()
    try:
        s.setup(a.wait)
        phases = {
            "edit": lambda: s.edit(a.seconds),
            "build": lambda: s.build(a.seconds),
            "browse": lambda: s.browse(a.seconds),
            "cleanup": lambda: s.cleanup(),
            "burst": lambda: s.burst(a.seconds),
        }
        order = ["edit", "build", "browse", "cleanup", "burst"] if a.phase == "all" else [a.phase]
        for name in order:
            phases[name]()
        if not a.keep:
            time.sleep(3)
            s.teardown()
    except KeyboardInterrupt:
        log("interrupted")
        if not a.keep:
            s.teardown()
    log(f"done: {s.ops} file operations in {time.time() - t0:.0f} s")


if __name__ == "__main__":
    main()
