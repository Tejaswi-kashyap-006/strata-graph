import { checkout } from "@/api";
import { Button } from "./Button";
import { Card } from "@shop/ui-kit";
import { bigButton } from "@shop/ui-kit/button";

export function App() {
  return (
    <Layout>
      <Button onClick={() => checkout([1])} />
      <Card />
      {bigButton()}
    </Layout>
  );
}

function Layout(props: { children?: unknown }) {
  return <div>{String(props.children)}</div>;
}
