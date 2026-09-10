import { GROUP_LABELS, TOOLS } from "../tools/registry";
import { setState, useStore } from "../state/store";

export function Rail() {
  const tool = useStore((s) => s.tool);
  const setupOpen = useStore((s) => s.setupOpen);
  const service = useStore((s) => s.service);
  const groups = Array.from(new Set(TOOLS.map((t) => t.group)));
  return (
    <nav className="rail" aria-label="Analyses">
      <div className="rail-groups">
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
      </div>
      <div className="rail-foot">
        <button
          type="button"
          className={`rail-setup${setupOpen ? " is-active" : ""}${!service ? " is-attention" : ""}`}
          onClick={() => setState({ setupOpen: !setupOpen })}
          title="Add street networks, profiles and GTFS feeds; see where files are saved"
        >
          <span className="rail-setup-icon" aria-hidden="true">⚙</span>
          Setup
          {!service && <span className="rail-setup-dot" aria-label="Setup needed" />}
        </button>
      </div>
    </nav>
  );
}
