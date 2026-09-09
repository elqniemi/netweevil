import { GROUP_LABELS, TOOLS } from "../tools/registry";
import { setState, useStore } from "../state/store";

export function Rail() {
  const tool = useStore((s) => s.tool);
  const groups = Array.from(new Set(TOOLS.map((t) => t.group)));
  return (
    <nav className="rail" aria-label="Analyses">
      {groups.map((group) => (
        <div className="rail-group" key={group}>
          <div className="rail-group-label">{GROUP_LABELS[group]}</div>
          {TOOLS.filter((t) => t.group === group).map((t) => (
            <button
              key={t.id}
              type="button"
              className={`rail-item${t.id === tool ? " is-active" : ""}`}
              onClick={() => setState({ tool: t.id, hoverInfo: null })}
              title={t.description}
            >
              {t.label}
            </button>
          ))}
        </div>
      ))}
    </nav>
  );
}
