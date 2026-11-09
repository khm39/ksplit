#!/usr/bin/env python3
"""Generate a large CSV for performance and resume testing.

Usage: gen_csv.py OUTPUT SIZE_BYTES [--seed N] [--quoted-ratio 0.05]
Columns: id (int, increasing), order_date (date), amount (decimal 2), customer (string),
note (sometimes quoted, may contain commas and newlines).
"""
import argparse
import os
import random
import sys


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output")
    parser.add_argument("size", type=int, help="target size in bytes")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--quoted-ratio", type=float, default=0.05)
    args = parser.parse_args()

    rng = random.Random(args.seed)
    customers = [f"cust-{i:05d}" for i in range(2000)]
    words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"]
    header = "id,order_date,amount,customer,note\n"
    written = 0
    row_id = 0
    with open(args.output, "w", encoding="utf-8", newline="") as f:
        f.write(header)
        written += len(header)
        buf = []
        while written < args.size:
            for _ in range(10000):
                row_id += 1
                day = rng.randrange(0, 365 * 3)
                y, rem = divmod(day, 365)
                m, d = divmod(rem, 28)
                date = f"{2023 + y:04d}-{m + 1:02d}-{d + 1:02d}"
                amount = f"{rng.randrange(0, 10_000_000) / 100:.2f}"
                customer = customers[rng.randrange(len(customers))]
                if rng.random() < args.quoted_ratio:
                    note = f'"{rng.choice(words)}, ""quoted""\n{rng.choice(words)}"'
                else:
                    note = rng.choice(words)
                buf.append(f"{row_id},{date},{amount},{customer},{note}\n")
            chunk = "".join(buf)
            buf.clear()
            f.write(chunk)
            written += len(chunk.encode("utf-8"))
    print(f"{args.output}: {written} bytes, {row_id} records", file=sys.stderr)


if __name__ == "__main__":
    main()
