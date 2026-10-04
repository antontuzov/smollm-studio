import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";

import { Button } from "./button";
import { toast } from "@/stores/ui";
import { cn } from "@/lib/utils";

/**
 * Copy text to the clipboard.
 *
 * The async clipboard is the first choice; WKWebView sometimes declines it for
 * a non-secure origin, so a synchronous selection is kept as the fallback.
 */
async function copyText(value: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(value);
      return true;
    }
  } catch (error) {
    console.warn("clipboard refused the async write", error);
  }
  try {
    const area = document.createElement("textarea");
    area.value = value;
    area.readOnly = true;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const copied = document.execCommand("copy");
    document.body.removeChild(area);
    return copied;
  } catch (error) {
    console.warn("clipboard fallback failed", error);
    return false;
  }
}

interface CopyButtonProps {
  text: string;
  label?: string;
  className?: string;
}

export function CopyButton({ text, label = "Copy", className }: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  return (
    <Button
      variant="ghost"
      size="icon-sm"
      aria-label={label}
      className={className}
      onClick={async () => {
        const ok = await copyText(text);
        if (!ok) {
          toast({ title: "Copy failed", description: "Select the text and copy it manually.", variant: "error" });
          return;
        }
        setCopied(true);
        window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setCopied(false), 1600);
      }}
    >
      {copied ? <Check className="text-success" /> : <Copy />}
    </Button>
  );
}

interface CodeBlockProps {
  code: string;
  /** Shown in the corner so a snippet says what language it is. */
  language?: string;
  caption?: string;
  className?: string;
}

export function CodeBlock({ code, language, caption, className }: CodeBlockProps) {
  return (
    <figure className={cn("overflow-hidden rounded-lg border bg-background/70", className)}>
      <figcaption className="flex items-center justify-between gap-2 border-b px-3 py-1.5">
        <span className="truncate font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
          {language ?? caption ?? "snippet"}
        </span>
        <CopyButton text={code} />
      </figcaption>
      <pre className="max-h-72 overflow-auto px-3 py-3 font-mono text-xs leading-relaxed">
        {code}
      </pre>
    </figure>
  );
}
