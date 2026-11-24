"""Guest-side VM samples. Byte counters are taken outside the containers."""
import json
import os
import sqlite3
import sys
import time
from pathlib import Path

DATABASE = Path.home() / ".modelcat-monitor" / "usage.db"
FIELDS = "stamp,segment,cpu,mem,rx,tx,rd,wr,free,total,ram"
INTERVAL = 5


def counters():
    cpu = [int(v) for v in Path("/proc/stat").read_text().splitlines()[0].split()[1:9]]
    device = os.stat("/").st_dev
    disk = Path(f"/sys/dev/block/{os.major(device)}:{os.minor(device)}").resolve()
    if (disk / "partition").exists():
        disk = disk.parent
    stats = (disk / "stat").read_text().split()
    rx = tx = 0
    # Guest NICs have a hardware device. Skip Docker bridges, veth and loopback.
    for nic in Path("/sys/class/net").iterdir():
        if (nic / "device").exists():
            rx += int((nic / "statistics/rx_bytes").read_text())
            tx += int((nic / "statistics/tx_bytes").read_text())
    return sum(cpu), cpu[3] + cpu[4], rx, tx, int(stats[2]) * 512, int(stats[6]) * 512


def collect():
    db = sqlite3.connect(DATABASE)
    db.execute("pragma journal_mode=WAL")
    db.execute("pragma synchronous=NORMAL")
    db.execute("create table if not exists samples(stamp integer primary key,segment integer,cpu real,mem integer,rx real,tx real,rd real,wr real,free integer,total integer,ram integer)")
    previous, previous_time = counters(), time.monotonic()
    segment = int(time.time())
    last_wall = segment
    while True:
        time.sleep(INTERVAL)
        current, instant, stamp = counters(), time.monotonic(), int(time.time())
        dt = instant - previous_time
        total, idle, *io = [a - b for a, b in zip(current, previous)]
        previous, previous_time = current, instant
        gap = stamp - last_wall
        last_wall = stamp
        if total <= 0 or min(io) < 0 or not 0 < gap < INTERVAL * 3 or dt > INTERVAL * 3:
            segment = stamp
            continue
        memory = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
        ram, available = [int(memory[k].split()[0]) * 1024 for k in ("MemTotal", "MemAvailable")]
        fs = os.statvfs(Path.home())
        values = (stamp, segment, max(0, min(100, 100 * (total-idle)/total)), ram-available,
                  *(v/dt for v in io), fs.f_bavail*fs.f_frsize, fs.f_blocks*fs.f_frsize, ram)
        with db:
            db.execute("insert or replace into samples values(?,?,?,?,?,?,?,?,?,?,?)", values)
            db.execute("delete from samples where stamp < ?", (stamp-86400,))


def history(seconds):
    if seconds not in (0, 300, 3600, 86400):
        raise ValueError("invalid time range")
    now = int(time.time())
    if not DATABASE.exists():
        return {"now": now, "latest": None, "points": []}
    db = sqlite3.connect(f"{DATABASE.as_uri()}?mode=ro", uri=True)
    if not db.execute("select 1 from sqlite_master where name='samples'").fetchone():
        return {"now": now, "latest": None, "points": []}
    db.row_factory = sqlite3.Row
    latest = db.execute("select " + FIELDS + " from samples order by stamp desc limit 1").fetchone()
    points = []
    if seconds:
        rows = db.execute("""select max(stamp) as stamp, segment, avg(cpu) as cpu, avg(mem) as mem,
            avg(rx) as rx, avg(tx) as tx, avg(rd) as rd, avg(wr) as wr
            from samples where stamp between ? and ? group by segment, cast(stamp / ? as integer)
            order by stamp""", (now-seconds, now, max(INTERVAL, seconds//180)))
        points = [dict(r) for r in rows]
    return {"now": now, "latest": dict(latest) if latest else None, "points": points}


if __name__ == "__main__":
    if sys.argv[1] == "collect":
        collect()
    else:
        print(json.dumps(history(int(sys.argv[1])), separators=(",", ":")))
