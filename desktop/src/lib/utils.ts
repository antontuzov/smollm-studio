import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** Tailwind-aware class joining, the shadcn/ui convention. */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

let idSequence = 0;

/**
 * Ids for generations and one-off dialogs. `crypto.randomUUID` is not
 * guaranteed in every webview, and these only need to be unique per process.
 */
export function nextId(prefix: string): string {
  idSequence += 1;
  return `${prefix}-${Date.now().toString(36)}-${idSequence}`;
}
