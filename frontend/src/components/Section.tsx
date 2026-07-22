import type { ReactNode } from "react";

interface Props {
  title: string;
  /** Shown in the header when collapsed: the settings inside, at a glance. */
  summary?: ReactNode;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}

/**
 * A collapsible sidebar section.
 *
 * The sidebar holds settings that are read far more often than they are changed, so each
 * group can fold away to a single row. What makes that safe rather than merely tidy is the
 * summary: a collapsed section still states what it is set to, so folding one never hides
 * the answer to "how is this configured" — only the controls that change it.
 */
export function Section({ title, summary, open, onToggle, children }: Props) {
  return (
    <div className={`section ${open ? "open" : ""}`}>
      <button
        className="section-head"
        onClick={onToggle}
        aria-expanded={open}
        title={open ? `Collapse ${title}` : `Expand ${title}`}
      >
        <span className="chev" aria-hidden="true" />
        <span className="title">{title}</span>
        {!open && summary && <span className="summary">{summary}</span>}
      </button>
      {open && <div className="section-body">{children}</div>}
    </div>
  );
}
