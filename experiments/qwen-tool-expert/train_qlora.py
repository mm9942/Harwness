#!/usr/bin/env python3
"""Opt-in QLoRA starter. Never allocates GPUs without an explicit --train."""
import argparse
import json
from pathlib import Path

BASE = "Qwen/Qwen3-1.7B"
ROOT = Path(__file__).resolve().parent

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir", default=str(ROOT / "data"))
    parser.add_argument("--output-dir", default=str(ROOT / "adapter"))
    parser.add_argument("--train", action="store_true", help="Actually load model and train (costs GPU time)")
    parser.add_argument("--allow-tiny-data", action="store_true", help="Development smoke test only")
    args = parser.parse_args()
    folder = Path(args.data_dir)
    train = [json.loads(x) for x in (folder / "train.jsonl").read_text().splitlines() if x.strip()]
    eval_rows = [json.loads(x) for x in (folder / "eval.jsonl").read_text().splitlines() if x.strip()]
    print(json.dumps({"model":BASE,"train":len(train),"eval":len(eval_rows),
                      "mode":"train" if args.train else "dry-run"},indent=2))
    if not args.train:
        print("Dry-run: no GPU instance, model download, or training launched.")
        return
    if len(train)<200 and not args.allow_tiny_data:
        raise SystemExit("Refusing to train on a tiny seed dataset. Generate >=200 reviewed training examples or pass --allow-tiny-data for a smoke test.")
    from datasets import Dataset
    from peft import LoraConfig
    from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig
    from trl import SFTConfig, SFTTrainer
    tokenizer = AutoTokenizer.from_pretrained(BASE, trust_remote_code=False)
    # Tool-call examples remain conversational to preserve the model's native
    # tool calling template. Never convert the calls to free-form pseudo JSON.
    model = AutoModelForCausalLM.from_pretrained(
        BASE, quantization_config=BitsAndBytesConfig(load_in_4bit=True,
        bnb_4bit_quant_type="nf4", bnb_4bit_compute_dtype=__import__("torch").bfloat16),
        device_map="auto", trust_remote_code=False)
    lora = LoraConfig(r=16,lora_alpha=32,lora_dropout=0.05,bias="none",
                      target_modules="all-linear",task_type="CAUSAL_LM")
    conf = SFTConfig(output_dir=args.output_dir, max_length=4096,
                     per_device_train_batch_size=1, gradient_accumulation_steps=8,
                     num_train_epochs=2, learning_rate=1e-4,
                     logging_steps=5, save_strategy="epoch",report_to="none",
                     assistant_only_loss=True, packing=False, bf16=True)
    # Enforce native assistant-token loss masking; fail rather than train on
    # user/tool messages if this template or TRL version lacks mask support.
    probe = tokenizer.apply_chat_template(
        train[0]["messages"], tools=train[0]["tools"], tokenize=True,
        return_dict=True, return_assistant_tokens_mask=True)
    mask = probe.get("assistant_masks")
    if not mask or not any(mask):
        raise SystemExit("Chat template did not produce an assistant loss mask. Stop and adapt template/TRL before training.")
    trainer = SFTTrainer(model=model, args=conf,
                         train_dataset=Dataset.from_list(train),
                         eval_dataset=Dataset.from_list(eval_rows),
                         peft_config=lora,processing_class=tokenizer)
    trainer.train()
    trainer.save_model(args.output_dir)
    print("LoRA adapter saved. No merge or production deployment was performed.")

if __name__=="__main__":
    main()
