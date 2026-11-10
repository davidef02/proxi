// The map: home route `#/`. Port of LearnHarness's knowledge-graph view (see GraphView.tsx).
import "./map.css";
import { GraphView } from "./GraphView.tsx";

export function MapPage() {
  return (
    <div className="lh-map h-full min-h-0 w-full">
      <GraphView />
    </div>
  );
}
