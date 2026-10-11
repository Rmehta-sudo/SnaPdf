#!/usr/bin/env python3
"""Persistent, local-only PaddleOCR worker using a JSON-lines protocol."""

import contextlib
import json
import os
import sys
import time

os.environ.setdefault(
    "PADDLE_PDX_CACHE_HOME",
    os.path.join(os.path.expanduser("~"), ".local", "share", "snapdf", "ocr", "models"),
)
os.environ.setdefault("PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK", "True")
os.environ.setdefault("FLAGS_use_mkldnn", "0")
os.environ.setdefault("OMP_NUM_THREADS", str(min(8, os.cpu_count() or 4)))

_ocr = None
_formula = None


def get_ocr():
    global _ocr
    if _ocr is None:
        with contextlib.redirect_stdout(sys.stderr):
            from paddleocr import PaddleOCR

            _ocr = PaddleOCR(
                text_detection_model_name="PP-OCRv5_mobile_det",
                text_recognition_model_name="PP-OCRv5_mobile_rec",
                use_doc_orientation_classify=False,
                use_doc_unwarping=False,
                use_textline_orientation=False,
                lang="en",
                device="cpu",
                enable_mkldnn=False,
                cpu_threads=min(8, os.cpu_count() or 4),
            )
    return _ocr


def get_formula():
    global _formula
    if _formula is None:
        with contextlib.redirect_stdout(sys.stderr):
            from paddleocr import FormulaRecognitionPipeline

            _formula = FormulaRecognitionPipeline(
                formula_recognition_model_name="PP-FormulaNet-S",
                use_layout_detection=False,
                use_doc_orientation_classify=False,
                use_doc_unwarping=False,
                device="cpu",
                enable_mkldnn=False,
                cpu_threads=min(8, os.cpu_count() or 4),
            )
    return _formula


def collect_text(value):
    if isinstance(value, dict):
        if isinstance(value.get("rec_texts"), list):
            return [str(text) for text in value["rec_texts"] if str(text).strip()]
        if isinstance(value.get("rec_formula"), str) and value["rec_formula"].strip():
            return [value["rec_formula"]]
        output = []
        for child in value.values():
            output.extend(collect_text(child))
        return output
    if isinstance(value, (list, tuple)):
        output = []
        for child in value:
            output.extend(collect_text(child))
        return output
    return []


def recognize(path, mode):
    started = time.perf_counter()
    with contextlib.redirect_stdout(sys.stderr):
        results = list(
            (get_formula() if mode == "math" else get_ocr()).predict(input=path)
        )
    texts = []
    for result in results:
        payload = getattr(result, "json", result)
        if callable(payload):
            payload = payload()
        texts.extend(collect_text(payload))
    return {
        "text": "\n".join(texts),
        "engine": "PP-FormulaNet-S to LaTeX (CPU)" if mode == "math" else "PP-OCRv5 mobile (CPU)",
        "inference_ms": round((time.perf_counter() - started) * 1000),
    }


for line in sys.stdin:
    try:
        request = json.loads(line)
        if request.get("command") == "warmup":
            get_ocr()
            response = {"ready": True}
        elif request.get("command") == "recognize":
            response = recognize(request["path"], request.get("mode", "text"))
        else:
            response = {"error": "Unknown OCR command."}
    except Exception as error:
        response = {"error": f"Local OCR failed: {error}"}
    print(json.dumps(response, ensure_ascii=False), flush=True)
