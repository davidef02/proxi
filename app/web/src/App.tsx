import { useRoute } from "./router.ts";
import { Shell } from "./shell/Shell.tsx";
import { MapPage } from "./map/MapPage.tsx";
import { CoursePage } from "./pages/CoursePage.tsx";
import { SessionPage } from "./session/SessionPage.tsx";
import { LessonPage } from "./lesson/LessonPage.tsx";
import { ReferencePage } from "./lesson/DocBody.tsx";
import { ExercisePage } from "./exercise/ExercisePage.tsx";

export function App() {
  const route = useRoute();
  return (
    <Shell>
      {route.name === "home" && <MapPage />}
      {route.name === "course" && <CoursePage key={route.slug} slug={route.slug} />}
      {route.name === "session" && <SessionPage key={route.id} id={route.id} />}
      {route.name === "lesson" && <LessonPage key={route.path} path={route.path} node={route.node} />}
      {route.name === "reference" && <ReferencePage key={route.path} path={route.path} />}
      {route.name === "exercise" && <ExercisePage key={`${route.course}/${route.exercise}`} course={route.course} exercise={route.exercise} sessionId={route.sessionId} />}
    </Shell>
  );
}
