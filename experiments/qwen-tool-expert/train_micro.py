#!/usr/bin/env python3
"""Opt-in LoRA trainer for an individual 135M cycle micro-expert.

No network access, GPU allocation, or model download occurs during dry-run.
Uses the actual model's conversation template and fails closed without an
assistant-token mask instead of silently training on user text.
"""
import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--expert", default="intent")
    parser.add_argument("--data-dir", default=str(ROOT / "micro_data"))
    parser.add_argument("--output-dir", default=str(ROOT / "micro_adapters"))
    parser.add_argument("--train", action="store_true")
    parser.add_argument("--allow-tiny-data", action="store_true",
                        help="Permit small synthetic dataset for a smoke test only")
    parser.add_argument("--allow-cpu", action="store_true",
                        help="Explicitly allow slow CPU-only training")
    args = parser.parse_args()
    manifest = json.loads((ROOT / "micro_experts.json").read_text())
    pilot = {x["id"] for x in manifest["experts"] if x["pilot"]}
    if args.expert not in pilot:
        parser.error("Expert has no curated phase data yet; select from: " + ", ".join(sorted(pilot)))
    folder = Path(args.data_dir) / args.expert
    train = [json.loads(x) for x in (folder / "train.jsonl").read_text().splitlines() if x.strip()]
    evaluation = [json.loads(x) for x in (folder / "eval.jsonl").read_text().splitlines() if x.strip()]
    if not train or not evaluation:
        raise SystemExit("Train and evaluation splits are required")
    base = manifest["base_checkpoint"]
    print(json.dumps({"model": base, "expert": args.expert, "train": len(train),
                      "eval": len(evaluation),
                      "mode": "opt-in training" if args.train else "dry-run"}, indent=2))
    if not args.train:
        print("Dry-run only. No training, model download or paid GPU instance.")
        return
    if len(train) < 200 and not args.allow_tiny_data:
        raise SystemExit("Dataset is only a seed. Supply >=200 vetted examples or use "
                         "--allow-tiny-data for a correctness smoke test.")
    import torch
    if not torch.cuda.is_available() and not args.allow_cpu:
        raise SystemExit("CUDA GPU not detected; pass --allow-cpu only if slow CPU training is intended.")
    from datasets import Dataset
    from peft import LoraConfig
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from trl import SFTConfig, SFTTrainer

    tokenizer = AutoTokenizer.from_pretrained(base, trust_remote_code=False)
    probe = tokenizer.apply_chat_template(
        train[0]["messages"], tokenize=True, return_dict=True,
        return_assistant_tokens_mask=True
    )
    if not probe.get("assistant_masks") or not any(probe["assistant_masks"]):
        raise SystemExit("Model template lacks a valid assistant-generation mask; "
                         "create/test a compatible template before fitting LoRA.")
    dtype = (torch.bfloat16 if torch.cuda.is_available() and torch.cuda.is_bf16_supported()
             else torch.float16 if torch.cuda.is_available() else torch.float32)
    model = AutoModelForCausalLM.from_pretrained(
        base, torch_dtype=dtype, trust_remote_code=False
    )
    lora = LoraConfig(
        r=8, lora_alpha=16, lora_dropout=0.05,
        bias="none", target_modules="all-linear", task_type="CAUSAL_LM"
    )
    out = Path(args.output_dir) / args.expert
    settings = SFTConfig(
        output_dir=str(out), max_length=1024,
        per_device_train_batch_size=2 if torch.cuda.is_available() else 1,
        gradient_accumulation_steps=8, num_train_epochs=2,
        learning_rate=2e-4, save_strategy="epoch", logging_steps=10,
        assistant_only_loss=True, packing=False, report_to="none",
        bf16=dtype==torch.bfloat16, fp16=dtype==torch.float16,
    )
    only_messages = lambda rows: Dataset.from_list([{"messages": x["messages"]} for x in rows])
    trainer = SFTTrainer(model=model, args=settings,
                         train_dataset=only_messages(train),
                         eval_dataset=only_messages(evaluation),
                         peft_config=lora, processing_class=tokenizer)
    trainer.train()
    trainer.save_model(str(out))
    print("Micro-adapter saved, no deployment or model merge performed.")

if __name__ == "__main__":
    main()
