import React from "react";

export default function ({ title }: { title: string }) {
  return (
    <section className="card">
      Shelf   of
      {title}
    </section>
  );
}

export const Badge = (props: { count: number }) => <b>{props.count}</b>;

export class Panel extends React.Component<{ name: string }> {
  render() {
    return <div>{`panel ${this.props.name}`}</div>;
  }
}
