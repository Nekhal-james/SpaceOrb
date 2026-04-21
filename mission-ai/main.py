"""
SpaceOrb V7.6 — AI Sandbox (mission-ai)

Flask-based ingest service with YOLOv8 inference and UDS IPC to the Rust supervisor.
Implements RAM shield backpressure (HTTP 429 if /mnt/ram_shield > 80%).

Reference: SPACEORB_CORE_SPEC.txt §3.2, §3.3
"""

import json
import logging
import os
import shutil
import socket
import struct
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from flask import Flask, Response, jsonify, request

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

RAM_SHIELD_PATH = os.getenv("RAM_SHIELD_PATH", "/mnt/ram_shield")
RAM_SHIELD_LIMIT_GB = float(os.getenv("RAM_SHIELD_LIMIT_GB", "1.0"))
RAM_SHIELD_BACKPRESSURE_PERCENT = float(os.getenv("RAM_SHIELD_BACKPRESSURE_PERCENT", "80"))
IPC_SOCKET_PATH = os.getenv("IPC_SOCKET_PATH", "/tmp/mission.sock")
YOLO_MODEL_PATH = os.getenv("YOLO_MODEL_PATH", "yolov8n.pt")
INGEST_PORT = int(os.getenv("INGEST_PORT", "5050"))
LOG_LEVEL = os.getenv("LOG_LEVEL", "INFO")

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------

logging.basicConfig(
    level=getattr(logging, LOG_LEVEL.upper(), logging.INFO),
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    handlers=[logging.StreamHandler(sys.stdout)],
)
logger = logging.getLogger("mission-ai")

# ---------------------------------------------------------------------------
# YOLOv8 Model Loader
# ---------------------------------------------------------------------------

_yolo_model = None


def _load_yolo_model():
    """Load the YOLOv8 model. Returns None if ultralytics is unavailable."""
    global _yolo_model
    if _yolo_model is not None:
        return _yolo_model

    try:
        from ultralytics import YOLO

        model_path = Path(YOLO_MODEL_PATH)
        if model_path.exists():
            _yolo_model = YOLO(str(model_path))
            logger.info(f"YOLOv8 model loaded from {model_path}")
        else:
            # Download the default nano model
            _yolo_model = YOLO("yolov8n.pt")
            logger.info("YOLOv8 nano model downloaded and loaded")
        return _yolo_model
    except ImportError:
        logger.warning(
            "ultralytics not installed — running in mock inference mode"
        )
        return None
    except Exception as e:
        logger.error(f"Failed to load YOLOv8 model: {e}")
        return None


# ---------------------------------------------------------------------------
# RAM Shield Backpressure
# ---------------------------------------------------------------------------


def check_ram_shield_utilization() -> float:
    """
    Check /mnt/ram_shield utilization as a percentage.

    Returns a float between 0.0 and 100.0.
    If the path doesn't exist (dev environment), returns 0.0.
    """
    try:
        usage = shutil.disk_usage(RAM_SHIELD_PATH)
        if usage.total == 0:
            return 0.0
        return (usage.used / usage.total) * 100.0
    except FileNotFoundError:
        logger.debug(
            f"RAM shield path {RAM_SHIELD_PATH} not found (dev environment)"
        )
        return 0.0
    except OSError as e:
        logger.error(f"Failed to check RAM shield utilization: {e}")
        return 0.0


def is_backpressure_active() -> bool:
    """Return True if RAM shield utilization exceeds the threshold."""
    utilization = check_ram_shield_utilization()
    if utilization > RAM_SHIELD_BACKPRESSURE_PERCENT:
        logger.warning(
            f"RAM shield at {utilization:.1f}% "
            f"(limit: {RAM_SHIELD_BACKPRESSURE_PERCENT}%) — backpressure active"
        )
        return True
    return False


# ---------------------------------------------------------------------------
# IPC — Unix Domain Socket Client
# ---------------------------------------------------------------------------


def send_to_supervisor(payload: dict) -> bool:
    """
    Send a JSON payload to the Rust supervisor via UDS.

    Each message is a newline-terminated JSON string.

    Returns True on success, False on failure.
    """
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(5.0)
        sock.connect(IPC_SOCKET_PATH)

        message = json.dumps(payload, default=str) + "\n"
        sock.sendall(message.encode("utf-8"))
        sock.close()

        logger.debug(f"Sent to supervisor: criticality={payload.get('criticality')}")
        return True
    except FileNotFoundError:
        logger.error(
            f"Supervisor socket not found at {IPC_SOCKET_PATH} — "
            "is mission-core running?"
        )
        return False
    except ConnectionRefusedError:
        logger.error("Supervisor refused connection — is mission-core running?")
        return False
    except socket.timeout:
        logger.error("Supervisor IPC timed out (5s)")
        return False
    except OSError as e:
        logger.error(f"IPC error: {e}")
        return False


# ---------------------------------------------------------------------------
# Inference Pipeline
# ---------------------------------------------------------------------------


def run_inference(image_data: bytes, filename: str) -> dict:
    """
    Run YOLOv8 inference on the provided image.

    Returns a dict with:
    - criticality: 1000 for anomaly, 1 for routine
    - detection_metadata: detection results
    - timestamp: ISO 8601
    """
    model = _load_yolo_model()
    timestamp = datetime.now(timezone.utc).isoformat()

    if model is None:
        # Mock inference mode
        logger.info(f"Mock inference on {filename}")
        return {
            "criticality": 1,
            "detection_metadata": {
                "mode": "mock",
                "filename": filename,
                "detections": [],
                "message": "YOLOv8 not available — mock result",
            },
            "timestamp": timestamp,
        }

    try:
        # Save to RAM shield for processing
        ram_path = Path(RAM_SHIELD_PATH) / f"ingest_{filename}"
        ram_path.parent.mkdir(parents=True, exist_ok=True)
        ram_path.write_bytes(image_data)

        # Run inference
        results = model(str(ram_path), verbose=False)

        # Parse detections
        detections = []
        criticality = 1  # Default: Routine

        for result in results:
            if result.boxes is not None:
                for box in result.boxes:
                    det = {
                        "class": int(box.cls[0].item()),
                        "class_name": model.names.get(int(box.cls[0].item()), "unknown"),
                        "confidence": float(box.conf[0].item()),
                        "bbox": box.xyxy[0].tolist(),
                    }
                    detections.append(det)

                    # If any detection has high confidence, mark as anomaly
                    if det["confidence"] > 0.85:
                        criticality = 1000

        # Clean up staged file
        try:
            ram_path.unlink()
        except OSError:
            pass

        logger.info(
            f"Inference complete: {len(detections)} detections, "
            f"criticality={criticality}"
        )

        return {
            "criticality": criticality,
            "detection_metadata": {
                "mode": "yolov8",
                "filename": filename,
                "detections": detections,
                "detection_count": len(detections),
                "model": YOLO_MODEL_PATH,
            },
            "timestamp": timestamp,
        }

    except Exception as e:
        logger.error(f"Inference failed: {e}")
        return {
            "criticality": 1,
            "detection_metadata": {
                "mode": "error",
                "filename": filename,
                "error": str(e),
            },
            "timestamp": timestamp,
        }


# ---------------------------------------------------------------------------
# Flask Application
# ---------------------------------------------------------------------------

app = Flask(__name__)


@app.route("/health", methods=["GET"])
def health():
    """Health check endpoint."""
    ram_util = check_ram_shield_utilization()
    return jsonify(
        {
            "status": "ok",
            "service": "mission-ai",
            "version": "7.6.0",
            "ram_shield_utilization": round(ram_util, 2),
            "backpressure_active": ram_util > RAM_SHIELD_BACKPRESSURE_PERCENT,
            "yolo_loaded": _yolo_model is not None,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }
    )


@app.route("/ingest", methods=["POST"])
def ingest():
    """
    Ingest an image for AI inference.

    Accepts multipart/form-data with an 'image' field.

    Returns:
    - 200: Inference result with criticality and detection_metadata
    - 429: Backpressure — RAM shield utilization exceeds threshold
    - 400: Bad request (missing image)
    - 500: Internal error
    """
    # Backpressure gate: return HTTP 429 if RAM shield > 80%
    if is_backpressure_active():
        return Response(
            json.dumps(
                {
                    "error": "backpressure",
                    "message": (
                        f"RAM shield utilization exceeds "
                        f"{RAM_SHIELD_BACKPRESSURE_PERCENT}% — "
                        "try again later"
                    ),
                    "ram_shield_utilization": check_ram_shield_utilization(),
                }
            ),
            status=429,
            mimetype="application/json",
        )

    # Extract image from request
    if "image" not in request.files:
        # Try raw body
        image_data = request.get_data()
        if not image_data:
            return jsonify({"error": "No image provided"}), 400
        filename = f"raw_{int(time.time() * 1000)}.bin"
    else:
        image_file = request.files["image"]
        image_data = image_file.read()
        filename = image_file.filename or f"upload_{int(time.time() * 1000)}.jpg"

    if len(image_data) == 0:
        return jsonify({"error": "Empty image data"}), 400

    logger.info(f"Ingest request: {filename} ({len(image_data)} bytes)")

    # Run inference
    result = run_inference(image_data, filename)

    # Send result to Rust supervisor via IPC
    ipc_success = send_to_supervisor(result)

    # Include IPC status in response
    result["ipc_forwarded"] = ipc_success

    status_code = 200
    if result["criticality"] >= 1000:
        logger.warning(f"ANOMALY DETECTED: {filename}")
        status_code = 200  # Still 200, but the criticality field signals anomaly

    return jsonify(result), status_code


@app.route("/ingest/batch", methods=["POST"])
def ingest_batch():
    """
    Batch ingest multiple images.

    Accepts multipart/form-data with multiple 'images' fields.
    """
    if is_backpressure_active():
        return Response(
            json.dumps({"error": "backpressure"}),
            status=429,
            mimetype="application/json",
        )

    files = request.files.getlist("images")
    if not files:
        return jsonify({"error": "No images provided"}), 400

    results = []
    for image_file in files:
        image_data = image_file.read()
        filename = image_file.filename or f"batch_{int(time.time() * 1000)}.jpg"

        result = run_inference(image_data, filename)
        ipc_success = send_to_supervisor(result)
        result["ipc_forwarded"] = ipc_success
        results.append(result)

    return jsonify({"results": results, "count": len(results)}), 200


# ---------------------------------------------------------------------------
# Entry Point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    logger.info("=== SpaceOrb V7.6 AI Sandbox ===")
    logger.info(f"RAM Shield: {RAM_SHIELD_PATH} (limit: {RAM_SHIELD_LIMIT_GB}GB)")
    logger.info(f"IPC Socket: {IPC_SOCKET_PATH}")
    logger.info(f"YOLO Model: {YOLO_MODEL_PATH}")
    logger.info(f"Listening on port {INGEST_PORT}")

    # Pre-load model at startup
    _load_yolo_model()

    app.run(
        host="0.0.0.0",
        port=INGEST_PORT,
        debug=False,
        threaded=True,
    )
