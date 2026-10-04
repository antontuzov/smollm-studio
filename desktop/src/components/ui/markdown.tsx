import { useMemo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { PrismLight as SyntaxHighlighter } from "react-syntax-highlighter";
import bash from "react-syntax-highlighter/dist/esm/languages/prism/bash";
import json from "react-syntax-highlighter/dist/esm/languages/prism/json";
import python from "react-syntax-highlighter/dist/esm/languages/prism/python";
import rust from "react-syntax-highlighter/dist/esm/languages/prism/rust";
import typescript from "react-syntax-highlighter/dist/esm/languages/prism/typescript";
import javascript from "react-syntax-highlighter/dist/esm/languages/prism/javascript";
import { oneDark, oneLight } from "react-syntax-highlighter/dist/esm/styles/prism";

import { useUi, prefersDark } from "@/stores/ui";
import { cn } from "@/lib/utils";

import type { CSSProperties } from "react";
import type { Components } from "react-markdown";

/**
 * Only the languages a small local model plausibly answers with are
 * registered — PrismLight keeps the bundle honest.
 */
const registered = ["bash", "json", "python", "rust", "typescript", "javascript"];

SyntaxHighlighter.registerLanguage("bash", bash);
SyntaxHighlighter.registerLanguage("json", json);
SyntaxHighlighter.registerLanguage("python", python);
SyntaxHighlighter.registerLanguage("rust", rust);
SyntaxHighlighter.registerLanguage("typescript", typescript);
SyntaxHighlighter.registerLanguage("javascript", javascript);

const inlineCodeClass =
  "rounded border bg-secondary/60 px-1 py-0.5 font-mono text-[0.85em] text-foreground";

function buildComponents(dark: boolean): Components {
  return {
    code: ({ className, children }) => {
      const text = String(children ?? "").replace(/\n$/, "");
      const language = /language-(\w+)/.exec(className ?? "")?.[1];
      if (language && registered.includes(language)) {
        return (
          <SyntaxHighlighter
            language={language}
            style={dark ? oneDark : oneLight}
            PreTag="div"
            customStyle={{ margin: 0, fontSize: "12px", background: "transparent" }}
            codeTagProps={{ style: { fontFamily: "inherit" } as CSSProperties }}
          >
            {text}
          </SyntaxHighlighter>
        );
      }
      if (text.includes("\n")) {
        return <code className={cn("block font-mono text-xs leading-relaxed", className)}>{text}</code>;
      }
      return <code className={inlineCodeClass}>{text}</code>;
    },
    pre: ({ children }) => (
      <div className="my-2 overflow-x-auto rounded-lg border bg-background/70 px-3 py-2.5">
        {children}
      </div>
    ),
    a: ({ href, children }) => (
      // `target="_blank"` keeps the webview on this page: no window ever
      // navigates away from the app because a model said so.
      <a href={href} target="_blank" rel="noreferrer" className="text-accent underline underline-offset-2">
        {children}
      </a>
    ),
    p: ({ children }) => <p className="mb-2 last:mb-0">{children}</p>,
    ul: ({ children }) => <ul className="mb-2 list-disc space-y-1 pl-5 last:mb-0">{children}</ul>,
    ol: ({ children }) => <ol className="mb-2 list-decimal space-y-1 pl-5 last:mb-0">{children}</ol>,
    li: ({ children }) => <li className="leading-relaxed">{children}</li>,
    h1: ({ children }) => <h4 className="mb-1 mt-2 text-base font-semibold">{children}</h4>,
    h2: ({ children }) => <h4 className="mb-1 mt-2 text-base font-semibold">{children}</h4>,
    h3: ({ children }) => <h5 className="mb-1 mt-2 text-sm font-semibold">{children}</h5>,
    h4: ({ children }) => <h5 className="mb-1 mt-2 text-sm font-semibold">{children}</h5>,
    blockquote: ({ children }) => (
      <blockquote className="my-2 border-l-2 border-primary/50 pl-3 text-muted-foreground">
        {children}
      </blockquote>
    ),
    table: ({ children }) => (
      <div className="my-2 overflow-x-auto">
        <table className="w-full border-collapse text-xs">{children}</table>
      </div>
    ),
    th: ({ children }) => (
      <th className="border-b px-2 py-1.5 text-left font-semibold">{children}</th>
    ),
    td: ({ children }) => <td className="border-b px-2 py-1.5 align-top">{children}</td>,
    hr: () => <hr className="my-3" />,
  };
}

export function Markdown({ content, className }: { content: string; className?: string }) {
  const theme = useUi((state) => state.theme);
  const dark = prefersDark(theme);
  const components = useMemo(() => buildComponents(dark), [dark]);
  return (
    <div className={cn("text-sm leading-relaxed", className)}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {content}
      </ReactMarkdown>
    </div>
  );
}
