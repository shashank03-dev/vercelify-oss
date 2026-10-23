import { LazyMotion, domAnimation } from "framer-motion";
import { BeamTrack } from "./components/Beam";
import { Compare, Faq, FinalCta, Footer, Horizon } from "./components/Closing";
import { Features } from "./components/Features";
import { FromVercel } from "./components/FromVercel";
import { Hero, Nav } from "./components/Hero";
import { Stack, Statement } from "./components/Intro";
import { Hardware, Network } from "./components/Machine";
import { Steps } from "./components/Steps";

export default function App() {
  // Only the DOM animation features ship (m.* components, not motion.*).
  return (
    <LazyMotion features={domAnimation} strict>
      <Nav />
      <main>
        <Hero />
        {/* The beam leaves the hero's app window and runs down to the horizon. */}
        <BeamTrack>
          <Stack />
          <Statement />
          <Steps />
          <Features />
          <FromVercel />
          <Hardware />
          <Network />
          <Compare />
          <Faq />
          <FinalCta />
          <Footer />
        </BeamTrack>
        <Horizon />
      </main>
    </LazyMotion>
  );
}
