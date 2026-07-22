import { useEffect, useRef, type ReactNode } from "react";

interface Props {
  title: string;
  /** Optional line under the title saying what the dialog is for. */
  subtitle?: ReactNode;
  onClose: () => void;
  /** True while work is in flight — closing is still allowed, but Escape is not. */
  busy?: boolean;
  children: ReactNode;
  footer?: ReactNode;
}

/**
 * A modal dialog: dimmed backdrop, Escape to dismiss, click outside to dismiss.
 *
 * Escape is ignored while `busy`, so a keystroke cannot pull the dialog out from under a
 * transfer that is already running on the instrument.
 */
export function Modal({ title, subtitle, onClose, busy, children, footer }: Props) {
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  // Move focus into the dialog so the keyboard lands somewhere sensible rather than staying
  // on the button that opened it, behind the backdrop. The panel itself takes it rather than
  // the first control — landing on Close reads as if dismissing were the expected action,
  // and on a destructive control it would be worse.
  useEffect(() => {
    panel.current?.focus();
  }, []);

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        ref={panel}
      >
        <div className="modal-head">
          <div className="titles">
            <div className="title">{title}</div>
            {subtitle && <div className="subtitle">{subtitle}</div>}
          </div>
          <button className="modal-close" onClick={onClose} title="Close">
            ×
          </button>
        </div>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-foot">{footer}</div>}
      </div>
    </div>
  );
}
