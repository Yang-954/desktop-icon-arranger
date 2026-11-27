"""实测 WebView2 用户数据目录的磁盘占用与增长曲线。

步骤:
  1. 清空 EBWebView 目录
  2. 反复启动/关闭 GUI, 每次记录目录大小与文件数
  3. 报告增长趋势

用法: python tools/measure_cache.py [轮数]
"""

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(r"D:\yang\日常非项目活动\code\desktop-icon-arranger")
EXE = REPO / "target" / "release" / "desktop-icon-arranger.exe"
DATA_DIR = Path(os.environ["LOCALAPPDATA"]) / "com.yang954.desktopiconarranger"
EB = DATA_DIR / "EBWebView"


def size_of(p: Path) -> tuple[int, int]:
    """返回 (总字节, 文件数)。"""
    total = 0
    count = 0
    if not p.exists():
        return 0, 0
    for f in p.rglob("*"):
        if f.is_file():
            try:
                total += f.stat().st_size
                count += 1
            except OSError:
                pass
    return total, count


def mb(n: int) -> str:
    return f"{n / 1024 / 1024:7.2f} MB"


def main() -> int:
    rounds = int(sys.argv[1]) if len(sys.argv) > 1 else 5

    print("=== 初始状态 ===")
    sz, cnt = size_of(DATA_DIR)
    print(f"  {DATA_DIR}")
    print(f"  大小 {mb(sz)}   文件 {cnt} 个")

    print("\n=== 清空 ===")
    shutil.rmtree(DATA_DIR, ignore_errors=True)
    print("  已删除整个目录, 从零开始测")

    env = dict(os.environ)
    env["QTWEBENGINE_DISABLE_SANDBOX"] = "1"

    history = []
    for i in range(rounds):
        proc = subprocess.Popen(
            [str(EXE)], cwd=str(EXE.parent), env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        time.sleep(9)
        proc.kill()
        try:
            proc.wait(timeout=5)
        except Exception:
            pass
        time.sleep(1.2)
        sz, cnt = size_of(DATA_DIR)
        history.append((i + 1, sz, cnt))
        print(f"  第 {i+1} 次运行后: {mb(sz)}   {cnt} 个文件")

    print("\n=== 增长曲线 ===")
    prev = 0
    for i, sz, cnt in history:
        delta = sz - prev
        print(f"  run{i}: {mb(sz)}   (本次 +{delta/1024/1024:.2f} MB)   {cnt} 个文件")
        prev = sz

    if len(history) >= 2:
        first_delta = history[1][1] - history[0][1]
        last_delta = history[-1][1] - history[-2][1]
        print(f"\n  首次增量: {first_delta/1024/1024:.2f} MB")
        print(f"  末次增量: {last_delta/1024/1024:.2f} MB")
        if last_delta <= 64 * 1024:
            print("  => 末次增量已接近 0, 说明**已收敛, 不会无限增长**")
        else:
            print("  => 仍在增长, 需要继续观察")

    print("\n=== 目录明细 ===")
    if EB.exists():
        for sub in sorted(EB.iterdir()):
            if sub.is_dir():
                s, c = size_of(sub)
                print(f"  {mb(s)}  {c:>4} 个  {sub.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
