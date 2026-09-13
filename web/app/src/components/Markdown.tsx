import { memo, useState, type ComponentProps, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "@/components/ui/button";

// react-markdown never renders raw HTML (no rehype-raw); links are limited to safe schemes.
const SAFE_URL = /^(https?:|mailto:)/i;
const urlTransform = (url: string): string => (SAFE_URL.test(url) ? url : "");

export function useCopy(): [boolean, (text: string) => void] {
  const [copied, setCopied] = useState(false);
  const copy = (text: string) => {
    navigator.clipboard?.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }).catch(() => undefined);
  };
  return [copied, copy];
}

export function CopyButton({ text, label = "Copiază" }: { text: string; label?: string }) {
  const [copied, copy] = useCopy();
  return (
    <Button type="button" variant="ghost" size="xs" aria-label={label} onClick={() => copy(text)}
      className="text-muted-foreground hover:text-foreground">
      {copied ? <CheckIcon aria-hidden /> : <CopyIcon aria-hidden />}
      <span className="sr-only">{copied ? "Copiat" : label}</span>
    </Button>
  );
}

function textOf(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (node && typeof node === "object" && "props" in node) {
    return textOf((node as { props: { children?: ReactNode } }).props.children);
  }
  return "";
}

export function CodeBlock({ code, language }: { code: string; language?: string }) {
  return (
    <div className="group/code relative my-2 max-w-full overflow-hidden rounded-lg border border-border bg-card">
      <div className="flex h-8 items-center justify-between border-b border-border px-3 text-xs text-muted-foreground">
        <span className="font-mono">{language || "cod"}</span>
        <CopyButton text={code} label="Copiază codul" />
      </div>
      <pre className="overflow-x-auto p-3 font-mono text-[13px] leading-relaxed" tabIndex={0}>
        <code>{code}</code>
      </pre>
    </div>
  );
}

const components: ComponentProps<typeof ReactMarkdown>["components"] = {
  pre: ({ children }) => {
    const child = Array.isArray(children) ? children[0] : children;
    const props = (child && typeof child === "object" && "props" in child
      ? (child as { props: { className?: string; children?: ReactNode } }).props : {}) as {
      className?: string; children?: ReactNode;
    };
    const language = /language-([\w-]+)/.exec(props.className ?? "")?.[1];
    return <CodeBlock code={textOf(props.children).replace(/\n$/, "")} language={language} />;
  },
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noopener noreferrer nofollow">{children}</a>
  ),
  img: ({ alt, src }) => <span className="text-muted-foreground">[imagine{alt ? `: ${alt}` : ""}{src ? ` ${src}` : ""}]</span>,
};

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="prose-mono text-[15px]">
      <ReactMarkdown remarkPlugins={[remarkGfm]} urlTransform={urlTransform} components={components}>
        {text}
      </ReactMarkdown>
    </div>
  );
});
