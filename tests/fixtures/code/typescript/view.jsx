export default {
  name: 'view',
  render() {
    return <p>A view</p>;
  },
};

export function Title({ text }) {
  return <h1>{text}</h1>;
}
