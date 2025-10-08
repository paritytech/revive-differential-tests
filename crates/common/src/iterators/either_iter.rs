/// An iterator that could be either of two iterators.
#[derive(Clone, Debug)]
pub enum EitherIter<A, B> {
	A(A),
	B(B),
}

impl<A, B, T> Iterator for EitherIter<A, B>
where
	A: Iterator<Item = T>,
	B: Iterator<Item = T>,
{
	type Item = T;

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			EitherIter::A(iter) => iter.next(),
			EitherIter::B(iter) => iter.next(),
		}
	}
}
